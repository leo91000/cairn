import type { Locator, Page } from '@playwright/test'
import type { ExecutionNode } from '../../packages/contracts/nodes'
import { randomUUID } from 'node:crypto'
import { MAIN_AGENT_ID } from '../../packages/contracts/constants'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

function escape(text: string) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** A machine's row in the fleet list. */
function nodeRow(page: Page, name: string) {
  return page.getByRole('list', { name: 'Machines' }).getByRole('button', { name: new RegExp(`^${escape(name)},`) })
}

/** Opens a machine's side panel on one of its sections. */
async function openNode(page: Page, name: string, section?: 'Overview' | 'Access' | 'Storage' | 'Settings') {
  await nodeRow(page, name).click()
  const panel = page.getByRole('dialog', { name: new RegExp(`^${escape(name)}`) })
  await expect(panel).toBeVisible()
  if (section)
    await showSection(panel, section)
  return panel
}

async function showSection(panel: Locator, section: 'Overview' | 'Access' | 'Storage' | 'Settings') {
  await panel.getByRole('group', { name: 'Node sections' }).getByRole('button', { name: section, exact: true }).click()
}

/** The value beside a label in a panel's figures. */
function figure(region: Locator, label: string) {
  return region.locator(`dt:text-is("${label}")`).locator('xpath=following-sibling::dd[1]')
}

test('shows disk accounting and keeps the storage draft revision through inventory polling', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
  const node: ExecutionNode = {
    revision: 'fixture',
    createdAt: 0,
    systemTags: [],
    runtimeSlots: null,
    nativeEnabled: false,
    nativeAccounts: [],
    nativeReviewedAgents: [],
    nativeModeRequest: null,
    nativeModeObserved: null,
    nativeModeProposal: null,
    nativeModeStatus: null,
    exclusiveReservations: false,
    exclusiveReservation: null,
    exclusiveAgents: [],
    usage: null,
    budgetError: null,
    pressure: null,
    executionReady: false,
    maintenance: null,
    maintenanceError: null,
    maintenanceGeneration: null,
    maintenanceStartedAt: null,
    imageDigest: null,
    updateError: null,
    retainedRuns: null,
    diskGrowthProtocol: null,
    diskCapacity: null,
    occupiedSlots: 0,
    availableSlots: 4,
    agents: [],
    staleDisks: { count: 2, diskMiB: 3072 },
    diskAccounting: null,
    storage: {
      cacheMiB: 102400,
      memoryCacheMiB: 256,
      reserveMiB: 10240,
      reservePercent: 5,
      backupSeconds: 60,
      maxDirtySeconds: 300,
    },

    id: randomUUID(),
    name: 'Disk capacity fixture',
    local: false,
    accepting: true,
    revoked: false,
    status: 'online',
    tags: [],
    capabilities: {
      backend: 'firecracker',
      ublk: false,
      nativeModeProtocol: 0,
      diskTotalMiB: 0,
      cpu: 8,
      memoryMiB: 16384,
      diskMiB: 92160,
      os: 'linux',
      arch: 'x86_64',
      kvm: true,

    },
    limits: { cpu: 4, memoryMiB: 8192, diskMiB: 102400 },
    slots: 4,
    runtimeId: 'development',
    lastSeen: Date.now(),
  }
  node.diskAccounting = {
    capturedAt: Date.now(),
    localBytes: 80 * 1024 ** 3,
    unusedGrantedBytes: 12 * 1024 ** 3,
    hostFreeBytes: 90 * 1024 ** 3,
    hostReserveBytes: 20 * 1024 ** 3,
    poolReserveBytes: 5 * 1024 ** 3,
    reclaimableBytes: 16 * 1024 ** 3,
    unsynchronizedBytes: 8 * 1024 ** 3,
  }
  node.exclusiveReservation = {
    id: 'reservation',
    nodeId: node.id,
    ownerRunId: randomUUID(),
    reason: 'GPU benchmark',
    state: 'acquired',
    createdAt: Date.now(),
    expiresAt: null,
    waitUntil: Date.now(),
    pausedRunIds: [],
    minimumAvailableMemoryMiB: 0,
  }
  await page.route('**/api/nodes', route => route.fulfill({ json: [node] }))
  const releases: string[] = []
  await page.route(`**/api/nodes/${node.id}/exclusive/reservation/release`, async (route) => {
    releases.push(route.request().method())
    await route.fulfill({ status: 503, json: { error: 'The node is unreachable.' } })
  })
  await page.goto(workspacePath('/nodes'))
  await workspace.signIn(page)
  const panel = await openNode(page, 'Disk capacity fixture')
  // A failed action is reported inside the panel, above the page it covers.
  await panel.getByRole('button', { name: 'Release reservation', exact: true }).click()
  await expect(panel.getByRole('alert')).toContainText('The node is unreachable.')
  expect(releases).toEqual(['POST'])
  await showSection(panel, 'Storage')
  await panel.getByRole('button', { name: 'Free old disks', exact: true }).click()
  const cleaning = page.getByRole('dialog', { name: 'Free old disks on Disk capacity fixture' })
  await expect(cleaning.getByRole('button', { name: 'Delete 3 GiB', exact: true })).toBeVisible()
  await cleaning.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await expect(cleaning).toHaveCount(0)
  await expect(panel).toBeVisible()
  const details = panel.getByRole('region', { name: 'Disk capacity details' })
  await expect(figure(details, 'Stored locally')).toHaveText('80 GiB')
  await expect(figure(details, 'Reclaimable, at least')).toHaveText('16 GiB')
  await expect(figure(details, 'Not yet synchronized')).toHaveText('8 GiB')
  await expect(details).toContainText('without the one-hour delay')
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(details).toBeVisible()
  expect(await details.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/nodes-disk-accounting-phone.png', fullPage: true })
  await panel.getByRole('button', { name: 'Edit storage settings', exact: true }).click()
  const editor = page.getByRole('dialog', { name: 'Storage · Disk capacity fixture' })
  await editor.getByLabel('Shared memory cache (MiB)', { exact: true }).fill('1024')
  const requests: { revision: string, storage: ExecutionNode['storage'] }[] = []
  await page.route(`**/api/nodes/${node.id}/storage`, async (route) => {
    const input = route.request().postDataJSON()
    requests.push(input)
    if (input.revision !== node.revision) {
      await route.fulfill({ status: 409, json: { error: 'The node configuration changed. Reopen the editor.' } })
      return
    }

    node.storage = input.storage
    node.revision = 'saved'
    await route.fulfill({ json: node })
  })
  // Another editor changes a field this draft has not touched.
  node.revision = 'remote-change'
  node.storage.reservePercent = 10
  await page.waitForResponse(async response => response.request().method() === 'GET'
    && new URL(response.url()).pathname.endsWith('/api/nodes')
    && (await response.json())[0]?.revision === 'remote-change', { timeout: 15000 })
  await expect(editor.getByLabel('Minimum free disk (%)', { exact: true })).toHaveValue('5')
  await editor.getByRole('button', { name: 'Save storage settings', exact: true }).click()
  await expect(editor.getByRole('alert')).toContainText('The node configuration changed')
  expect(requests[0]?.revision).toBe('fixture')
  expect(node.storage.reservePercent).toBe(10)
  await editor.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await panel.getByRole('button', { name: 'Edit storage settings', exact: true }).click()
  await expect(editor.getByLabel('Minimum free disk (%)', { exact: true })).toHaveValue('10')
  await editor.getByLabel('Shared memory cache (MiB)', { exact: true }).fill('1024')
  await editor.getByRole('button', { name: 'Save storage settings', exact: true }).click()
  await expect(editor).toHaveCount(0)
  expect(requests[1]?.revision).toBe('remote-change')
  expect(node.storage.reservePercent).toBe(10)
  expect(node.storage.memoryCacheMiB).toBe(1024)
})

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
  await page.getByText('Connect directly instead').click()
  await page.getByLabel('Direct manager address').fill(workspace.url)
  await page.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await page.getByLabel('Single-use enrollment code').textContent()
  expect(code).toHaveLength(43)
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code,
      name: 'Browser Linux',
      protocol: 1,
      runtimeId: 'development',
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
  const row = nodeRow(page, 'Browser Linux')
  await expect(row).toBeVisible()
  // Connected and set up: whatever the runtime reports, it is neither offline nor waiting for setup.
  await expect(row.locator('[data-state]')).not.toHaveAttribute('data-state', /^(offline|setup|revoked)$/)
  await expect(row).not.toContainText('No agent can use this machine yet')
  const summary = page.getByRole('region', { name: 'Fleet summary' })
  await expect(summary.locator('div').filter({ hasText: 'connected' }).locator('dd')).toHaveText('1 / 1')
  await expect(summary.locator('div').filter({ hasText: 'slots in use' }).locator('dd')).toHaveText('0 / 4')
  // It asks only once: the next refresh leaves the machine alone.
  await page.waitForResponse(reply => new URL(reply.url()).pathname.endsWith('/nodes') && reply.request().method() === 'GET', { timeout: 15000 })
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.screenshot({ path: 'test-results/nodes-desktop.png' })
  const node = await openNode(page, 'Browser Linux', 'Access')
  await expect(node.getByRole('list', { name: 'Agents allowed' })).toContainText('Main agent')
  await expect(node).not.toContainText('No agent can run here yet.')
  await expect(node.getByRole('button', { name: /Choose agents|^Agents$/ })).toHaveCount(0)
  await expect(node.getByRole('link', { name: 'Manage access from the agents ›' })).toBeVisible()
  await showSection(node, 'Overview')
  await page.screenshot({ path: 'test-results/nodes-desktop-panel.png' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: 'test-results/nodes-desktop-dark.png' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.screenshot({ path: 'test-results/nodes-phone.png', fullPage: true })
  // On phones a row keeps the name and the free slots; the name gets the width.
  const name = (await row.getByText('Browser Linux', { exact: true }).boundingBox())!
  expect(name.width).toBeGreaterThan(70)
  expect((await row.boundingBox())!.width).toBeLessThanOrEqual(390)
  await showSection(node, 'Storage')
  await node.getByRole('button', { name: 'Edit storage settings', exact: true }).click()
  const storage = page.getByRole('dialog', { name: 'Storage · Browser Linux' })
  await expect(storage.getByLabel('Clean disk cache budget (MiB)')).toBeVisible()
  await expect(storage.getByLabel('Shared memory cache (MiB)')).toHaveValue('256')
  await page.screenshot({ path: 'test-results/storage-phone.png' })
  await page.keyboard.press('Escape')
  await expect(storage).toHaveCount(0)
  await expect(node).toBeVisible()
  await showSection(node, 'Settings')
  await node.getByRole('button', { name: 'Configure' }).click()
  const dialog = page.getByRole('dialog', { name: 'Configure node' })
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
  await expect(figure(node, 'Shared CPU budget')).toHaveText('4 of 8 cores')
  await expect(figure(node, 'Tags')).toHaveText('fast · linux')
  await expect(figure(node, 'Execution slots')).toHaveText('12')
  await showSection(node, 'Overview')
  await expect(figure(node, 'CPU')).toContainText('4 cores')
  await expect(figure(node, 'Slots')).toContainText('12 of 12 free')
  const configured = (await workspace.api('/api/nodes')).find((value: { name: string }) => value.name === 'Browser Linux')
  expect(configured.limits.diskMiB).toBe(98304)
  await page.keyboard.press('Escape')
  await expect(node).toHaveCount(0)
  await page.getByRole('button', { name: 'Synchronization settings' }).click()
  const synchronization = page.getByRole('dialog', { name: 'S3 synchronization' })
  await synchronization.getByLabel('Synchronization target (seconds)').fill('45')
  await synchronization.getByLabel('Pause after disconnection (seconds)').fill('30')
  await expect(synchronization.getByLabel('Recovery points to retain')).toHaveCount(0)
  await synchronization.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(synchronization).toHaveCount(0)
  expect(await workspace.api('/api/nodes/settings')).toMatchObject({ intervalSeconds: 45, disconnectTimeoutSeconds: 30 })
  await openNode(page, 'Browser Linux', 'Settings')
  await node.getByRole('button', { name: 'Revoke', exact: true }).click()
  await page.getByRole('dialog', { name: 'Revoke node' }).getByRole('button', { name: 'Revoke node' }).click()
  await expect(node).toHaveCount(0)
  await expect(row).toHaveCount(0)
  // The dialog returns focus to the list the revoked machine left.
  await expect(page.getByRole('heading', { name: 'Machines', level: 2 })).toBeFocused()
  await page.getByRole('button', { name: 'Show revoked machines (1)' }).click()
  await expect(row).toContainText('Revoked')
  await openNode(page, 'Browser Linux')
  await expect(node).toContainText('This machine no longer has access')
  await expect(node.getByRole('button', { name: 'Configure' })).toHaveCount(0)
})

test('conversation placement distinguishes a preference from a strict pin and shows synchronization age', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
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
      runtimeId: 'development',
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
      data: { runtimeId: 'development', nodeProtocol: 2 },
    })
    expect(reply.ok()).toBe(true)
  }

  const destinationInvitation = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Destination server' })
  const destinationResponse = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code: destinationInvitation.code,
      name: 'Destination server',
      protocol: 1,
      runtimeId: 'development',
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
  // The placement choices share the panel's full width.
  const placement = panel.getByRole('group', { name: 'Placement', exact: true })
  const group = (await placement.boundingBox())!
  const fixed = (await placement.getByRole('button', { name: 'Fix to a node' }).boundingBox())!
  expect(group.x + group.width - (fixed.x + fixed.width)).toBeLessThan(5)
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
  // A legacy native file workspace is only saved once execution stops.
  workspace.service.store.updateRun(detail.run.id, {
    status: 'running',
    executionBackend: 'native',
    storage: { mode: 'native', localBytes: 191392768, dirtyBytes: 0 },
  } as any)
  await heartbeat()
  await expect(header).toContainText('Local work · saved when execution stops', { timeout: 15000 })
  // A running native ublk conversation synchronizes like a VM: its last sync stays visible.
  workspace.service.store.updateRun(detail.run.id, { storage: { mode: 'on-demand', localBytes: 191392768, dirtyBytes: 0 } } as any)
  await heartbeat()
  await expect(header).not.toContainText('saved when execution stops', { timeout: 15000 })
  await expect(header).toContainText('Synced 2 min ago')
  const nativePlacement = await workspace.api(`/api/nodes/placement/${detail.run.id}`)
  let privilegeStatus = 'pending'
  const privilegeGeneration = randomUUID()
  await page.route(`**/nodes/placement/${detail.run.id}`, route => route.fulfill({
    json: {
      ...nativePlacement,
      nodes: nativePlacement.nodes.map((candidate: { id: string }) => candidate.id !== node.nodeId
        ? candidate
        : {
            ...candidate,
            nativeModeStatus: privilegeStatus,
            nativeModeRequest: { mode: 'administrator', generation: privilegeGeneration },
            nativeModeObserved: {
              mode: privilegeStatus === 'applied' ? 'administrator' : 'protected',
              generation: privilegeStatus === 'applied' ? privilegeGeneration : null,
              status: privilegeStatus,
            },
          }),
    },
  }))
  await header.click()
  await expect(panel).toContainText('Last execution: Native protected · shared host GPU · files load on demand')
  await expect(panel).toContainText('Node privilege change in progress; current privileges are unconfirmed. New placements are blocked.')
  privilegeStatus = 'failed'
  await expect(panel).toContainText('Node privilege change failed; current privileges are unconfirmed. New placements are blocked.')
  privilegeStatus = 'applied'
  await expect(panel).toContainText('Confirmed node mode: Native administrator')
  await expect(panel).toContainText('Last execution: Native protected')
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
  await page.unroute(`**/nodes/placement/${detail.run.id}`)
  workspace.service.store.updateRun(detail.run.id, { status: 'succeeded', executionBackend: null } as any)
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
    const { agents } = await response.json()
    const main = agents.find((item: { id: string }) => item.id === MAIN_AGENT_ID)
    await route.fulfill({
      response,
      json: {
        agents: [...agents, {
          ...main,
          id: randomUUID(),
          name: 'Native helper',
          access: { ...main.access, backend: 'native', nodes: [randomUUID()] },
        }],
      },
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
  await page.getByText('Connect directly instead').click()
  await page.getByLabel('Direct manager address').fill(workspace.url)
  await page.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await page.getByLabel('Single-use enrollment code').textContent()
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code,
      name: 'Shared Linux',
      protocol: 1,
      runtimeId: 'development',
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
  }
  // A native agent already runs on another native machine.
  const laptopInvitation = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Native laptop', backend: 'native' })
  const laptop = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code: laptopInvitation.code,
      name: 'Native laptop',
      protocol: 1,
      runtimeId: 'development',
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
  await enrollment.getByText('Connect directly instead').click()
  await enrollment.getByLabel('Direct manager address').fill(workspace.url)
  await enrollment.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await enrollment.getByLabel('Single-use enrollment code').textContent()
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code,
      name: 'Native desktop',
      protocol: 1,
      runtimeId: 'development',
      capabilities: nativeCapabilities,
    },
  })
  expect(response.ok()).toBe(true)
  const desktop = (await response.json()).nodeId as string

  // Once connected, the machine is configured first: trusted people, then who may use it.
  const dialog = page.getByRole('dialog', { name: 'Configure node' })
  await expect(dialog).toBeVisible({ timeout: 20000 })
  await expect(dialog.getByLabel('Execution slots')).toBeEnabled()
  await dialog.getByLabel('Execution slots').fill('6')
  await expect(dialog.getByRole('group', { name: 'Exclusive reservations' })).toHaveCount(0)
  await expect(dialog.getByLabel('Enable native execution')).not.toBeChecked()
  await expect(dialog.getByRole('group', { name: 'Trusted people', exact: true })).toBeVisible()
  await expect(dialog).toContainText('All slots share these budgets.')
  // Older native runtimes need an update before the exclusive grant can be used.
  await expect(dialog).toContainText('needs an update before exclusive reservations')
  const callers = await workspace.api('/api/nodes/native-callers')
  expect(callers.length).toBeGreaterThan(0)
  await dialog.getByLabel(callers[0].email, { exact: true }).check()
  await dialog.getByLabel('Enable native execution').check()
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  const configured = (await workspace.api('/api/nodes')).find((value: { id: string }) => value.id === desktop)
  expect(configured).toMatchObject({ nativeEnabled: true, nativeAccounts: [callers[0].id], slots: 6 })

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
  await expect(nodeRow(page, 'Native desktop')).toContainText('ublk is unavailable')
  const node = await openNode(page, 'Native desktop')
  await expect(node).toContainText('Native process')
  await expect(node.getByRole('list', { name: 'Why Native desktop cannot take new work' })).toContainText('ublk is unavailable')
  await expect(figure(node, 'Platform')).toContainText('ublk unavailable')
  // Native ublk disks honor the node storage policy like VM disks.
  await showSection(node, 'Storage')
  await expect(node.getByRole('button', { name: 'Edit storage settings', exact: true })).toBeVisible()
  await showSection(node, 'Settings')
  await node.getByRole('button', { name: 'Revoke', exact: true }).click()
  const revoke = page.getByRole('dialog', { name: 'Revoke node', exact: true })
  await expect(revoke).toContainText('mounts=$(findmnt -rno TARGET)')
  await expect(revoke).toContainText('the command lists it and stops before deleting anything')
  await page.keyboard.press('Escape')
  await expect(revoke).toHaveCount(0)
  // Agents are granted on their own page; the panel lists them read-only.
  await showSection(node, 'Access')
  await expect(node.getByRole('list', { name: 'Agents allowed' })).toContainText(agent.name)
  await expect(node).toContainText('Enabled · 1 trusted person')
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).access.nodes).toContain(desktop)

  await showSection(node, 'Settings')
  await node.getByRole('button', { name: 'Configure', exact: true }).click()
  await expect(dialog.getByLabel('Execution slots')).toHaveValue('6')
  await expect(dialog.getByLabel(callers[0].email, { exact: true })).toBeChecked()
  await dialog.getByLabel('Enable native execution').uncheck()
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  // Configuring it again does not ask who may use it.
  await expect(grant).toHaveCount(0)
  expect((await workspace.api('/api/nodes')).find((value: { id: string }) => value.id === desktop).nativeEnabled).toBe(false)
  expect((await workspace.api('/api/nodes')).find((value: { id: string }) => value.id === desktop).slots).toBe(6)
})

test('switching a machine to native enrolls it again and retires the previous node', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Inspect the fixture' })
  // Only its placement matters here: earlier journeys may leave the main agent without a free node.
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.id, { timeout: 20000 }).toBeTruthy()
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const enroll = async (code: string, capabilities: Record<string, unknown>) => {
    const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
      data: {
        code,
        name: 'Workstation',
        protocol: 1,
        runtimeId: 'development',
        capabilities,
      },
    })
    expect(response.ok()).toBe(true)
    return (await response.json()).nodeId as string
  }

  const machine = {
    os: 'linux',
    arch: 'x86_64',
    cpu: 8,
    memoryMiB: 16384,
    diskMiB: 65536,
  }
  const vm = await enroll((await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Workstation' })).code, { ...machine, kvm: true })
  const main = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === MAIN_AGENT_ID)
  await workspace.api(`/api/agents/${MAIN_AGENT_ID}`, 'PUT', { ...main, access: { ...main.access, nodes: [...main.access.nodes, vm] } })
  await workspace.api(`/api/nodes/placement/${detail.run.id}`, 'PUT', { pinnedNodeId: vm, preferredNodeId: null })

  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/nodes'))
  await authenticateWorkspace(page)
  const workstation = await openNode(page, 'Workstation', 'Settings')
  await workstation.getByRole('button', { name: 'Switch to native', exact: true }).click()
  // The panel closes: the switch continues in its own steps.
  await expect(workstation).toHaveCount(0)
  const switching = page.getByRole('dialog', { name: 'Switch Workstation to native process', exact: true })
  await expect(switching.getByLabel('Machine name')).toHaveValue('Workstation')
  // The backend is the point of the switch: it is not offered again.
  await expect(switching.getByRole('combobox', { name: 'Execution backend' })).toHaveCount(0)
  await expect(switching).toContainText('weaker isolation')
  await switching.getByText('Connect directly instead').click()
  await switching.getByLabel('Direct manager address').fill(workspace.url)
  await switching.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await switching.getByLabel('Single-use enrollment code').textContent()
  // A different invitation can connect first, even with the same name and backend.
  const other = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Workstation', backend: 'native' })
  const unrelated = await enroll(other.code, {
    ...machine,
    backend: 'native',
    kvm: false,
  })
  await page.waitForResponse(async response => response.request().method() === 'GET'
    && new URL(response.url()).pathname.endsWith('/api/nodes')
    && (await response.json()).some((node: { id: string }) => node.id === unrelated))
  await expect(switching).toBeVisible()
  await expect(page.getByRole('dialog', { name: 'Configure node' })).toHaveCount(0)
  await expect(page.getByRole('dialog', { name: 'Retire the previous Firecracker VM node?', exact: true })).toHaveCount(0)
  const native = await enroll(code!, {
    ...machine,
    backend: 'native',
    kvm: false,
  })

  // The new runtime is configured and offered agents first, then the previous node can be retired.
  const configure = page.getByRole('dialog', { name: 'Configure node' })
  await expect(configure).toBeVisible({ timeout: 20000 })
  const callers = await workspace.api('/api/nodes/native-callers')
  await configure.getByLabel(callers[0].email, { exact: true }).check()
  await configure.getByLabel('Enable native execution').check()
  await configure.getByRole('button', { name: 'Save', exact: true }).click()
  const grant = page.getByRole('dialog', { name: 'Who can use “Workstation”?' })
  await grant.getByRole('button', { name: 'Later', exact: true }).click()
  const retire = page.getByRole('dialog', { name: 'Retire the previous Firecracker VM node?', exact: true })
  await expect(retire).toContainText('Workstation now runs as a native process node. Revoke the previous Firecracker VM node, Workstation, to finish the switch')
  await expect(retire).toContainText('Agents allowed on the previous node: Main agent.')
  await expect(retire).toContainText('Conversations fixed to or preferring the previous node return to automatic placement.')
  await expect(retire.getByLabel('Previous runtime uninstall command')).toContainText('sudo docker rm -f cairn-execution-node')
  await page.screenshot({ path: 'test-results/node-switch-retire.png' })
  const revoking = page.waitForRequest(request => request.method() === 'POST' && new URL(request.url()).pathname.endsWith(`/nodes/${vm}/revoke`))
  await retire.getByRole('button', { name: 'Revoke previous node', exact: true }).click()
  expect((await revoking).postDataJSON()).toEqual({ nodeId: vm, releasePlacements: true })
  await expect(retire).toHaveCount(0)
  await expect(page.getByText('Workstation revoked; 1 conversation returns to automatic placement')).toBeVisible()
  const nodes = await workspace.api('/api/nodes')
  expect(nodes.find((node: { id: string }) => node.id === vm).revoked).toBe(true)
  expect(nodes.find((node: { id: string }) => node.id === native).capabilities.backend).toBe('native')
  expect((await workspace.api(`/api/nodes/placement/${detail.run.id}`)).pinnedNodeId).toBeNull()
})

for (const automatic of [false, true]) {
  test(`existing conversation explicitly confirms backend migration and follows its progress (${automatic ? 'automatic' : 'fixed'})`, async ({ page, workspace }) => {
    test.setTimeout(60000)
    await useRelayForHttpMocks(page)
    // Earlier enrollment journeys may change Main's backend or allowed nodes.
    const agent = workspace.service.agent({ name: 'Migration source', provider: 'codex' })
    const chat = await workspace.api('/api/chats', 'POST', { agentId: agent.id })
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep this conversation' })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
    await page.route('**/nodes/placement/*', route => route.fulfill({ json: { nodes: [{ id: 'native-fixture', name: 'Desktop GPU' }], pinnedNodeId: null, preferredNodeId: null } }))
    let phase = 'ready'
    let requestBody: unknown
    await page.route('**/nodes/placement/*/backend-migration', async (route) => {
      if (route.request().method() === 'POST') {
        requestBody = route.request().postDataJSON()
        phase = 'pending'
        await route.fulfill({ json: { status: 'migrating' } })
        return
      }

      await route.fulfill({
        json: {
          chatId: chat.id,
          required: !automatic && phase !== 'complete',
          optional: automatic,
          fromBackend: phase === 'complete' ? 'native' : 'firecracker',
          toBackend: automatic && phase === 'complete' ? 'firecracker' : 'native',
          pending: phase === 'pending',
          nodes: [{ id: 'native-fixture', name: 'Desktop GPU' }],
        },
      })
    })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    await page.getByRole('button', { name: 'Execution node', exact: true }).click()
    await page.getByRole('button', { name: 'Migrate this conversation…', exact: true }).click()
    const confirm = page.getByRole('dialog', { name: 'Migrate this conversation?', exact: true })
    await expect(confirm).toContainText('fresh engine session')
    await expect(confirm).toContainText('original environment is retained')
    expect(requestBody).toBeUndefined()
    await page.screenshot({ path: 'test-results/backend-migration-confirm.png' })
    await confirm.getByRole('button', { name: 'Stop and migrate', exact: true }).click()
    await expect(page.getByRole('status').filter({ hasText: 'Migrating to native execution' })).toBeVisible()
    expect(requestBody).toEqual({ nodeId: 'native-fixture', backend: 'native', confirm: true })
    phase = 'complete'
    await expect(page.getByRole('status').filter({ hasText: 'Migration complete' })).toBeVisible()
    if (automatic) {
      await expect(page.getByRole('button', { name: 'Migrate this conversation…', exact: true })).toHaveCount(0)
      await page.getByRole('button', { name: 'Change environment again', exact: true }).click()
      await expect(page.getByText('You can explicitly change this conversation to a Firecracker VM.', { exact: false })).toBeVisible()
    }

    await expect(page).toHaveURL(/\/chats\//)
  })
}

test('previews node administrator powers and keeps failed transitions visibly blocked', async ({ page, workspace: _workspace }) => {
  await useRelayForHttpMocks(page)
  const node: any = {
    id: randomUUID(),
    name: 'Disposable administrator',
    local: false,
    accepting: true,
    status: 'online',
    revoked: false,
    nativeEnabled: true,
    nativeAccounts: ['trusted'],
    tags: [],
    slots: 2,
    occupiedSlots: 2,
    availableSlots: 0,
    executionReady: true,
    agents: [{ id: 'builder', name: 'Builder', allNodes: true }],
    capabilities: {
      os: 'linux',
      arch: 'x86_64',
      backend: 'native',
      ublk: true,
      nativeModeProtocol: 1,
      kvm: false,
      cpu: 4,
      memoryMiB: 8192,
      diskMiB: 65536,
    },
    limits: { cpu: 2, memoryMiB: 4096, diskMiB: 32768 },
    nativeModeObserved: { mode: 'protected', generation: null, status: 'applied' },
  }
  const requests: any[] = []
  await page.route('**/api/nodes', route => route.fulfill({ json: [node] }))
  await page.route(`**/nodes/${node.id}/native-mode`, async (route) => {
    const change = route.request().postDataJSON()
    requests.push(change)
    if (change.confirm) {
      node.nativeModeRequest = { mode: change.mode, generation: 'g1' }
      node.nativeModeStatus = 'failed'
      node.nativeModeObserved = {
        mode: 'protected',
        generation: null,
        status: 'failed',
        error: 'Stop confirmation unavailable',
      }
      return route.fulfill({ json: { kind: 'requested', node } })
    }

    return route.fulfill({
      json: {
        kind: 'preview',
        previewId: 'preview-one',
        preview: {
          mode: change.mode,
          warning: 'Unrestricted passwordless sudo on the real host. Root can read local credentials and alter Cairn. Returning to protected does not clean or restore this machine.',
          interruptions: 'Two conversations and their process descendants stop before restart. Files and history remain.',
          trustedAccounts: ['trusted'],
          agents: [{ id: 'builder', name: 'Builder' }],
          conversations: [{ id: 'c1', name: 'First conversation', status: 'running' }, { id: 'c2', name: 'Second conversation', status: 'running' }],
          tasks: [{ id: 't1', name: 'Nightly build' }],
        },
      },
    })
  })
  await page.goto(workspacePath('/nodes'))
  await authenticateWorkspace(page)
  const row = nodeRow(page, 'Disposable administrator')
  await expect(row).toContainText('Full')
  await expect(row).toContainText('All 2 slots in use')
  const card = await openNode(page, 'Disposable administrator', 'Access')
  await expect(card).toContainText('Native protected')
  await card.getByRole('button', { name: 'Switch to Native administrator', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Switch Disposable administrator to Native administrator?' })
  await expect(review).toContainText('real host')
  await expect(review).toContainText('First conversation')
  await expect(review).toContainText('Second conversation')
  await expect(review).toContainText('Nightly build')
  expect(requests).toEqual([{ mode: 'administrator', previewId: null, confirm: false }])
  await review.getByRole('button', { name: 'Confirm access and restart managed executions' }).click()
  await expect(review).toHaveCount(0)
  await expect(card).toContainText('Change failed; new placements are blocked')
  await expect(row).toContainText('The privilege change failed')
  await expect(page.getByRole('region', { name: 'Fleet summary' }).locator('div').filter({ hasText: 'attention' }).locator('dd')).toHaveText('1')
  await expect(card).toContainText('Last confirmed: Native protected')
  await expect(card).toContainText('Current privileges are unconfirmed')
  await expect(card).toContainText('Native protected')
  expect(requests[1]).toEqual({ mode: 'administrator', previewId: 'preview-one', confirm: true })
  await card.getByRole('button', { name: 'Switch to Native administrator', exact: true }).click()
  expect(requests.at(-1)).toEqual({ mode: 'administrator', previewId: null, confirm: false })
})
