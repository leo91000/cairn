import type { Page } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import { join } from 'node:path'
import process from 'node:process'
import { chromium, expect, test } from '@playwright/test'
import { executeOfficialSql, officialRelayFixture } from './official-relay-fixture'

test('a member mutation refused behind another member upload succeeds exactly once through relay', async () => {
  test.setTimeout(120000)
  const database = new URL(process.env.LEO_OFFICIAL_TEST_DATABASE_URL!)
  const schema = `capacity_${randomUUID().replaceAll('-', '')}`
  executeOfficialSql(database, `CREATE SCHEMA ${schema}`)
  const isolated = new URL(database)
  isolated.searchParams.set('options', `-c search_path=${schema}`)
  const fixture = await officialRelayFixture(4431)
  const browser = await chromium.launch({ args: ['--disable-features=WebRtcHideLocalIpsWithMdns'] })
  const { url, messages } = fixture
  const owner = await (await browser.newContext()).newPage()
  const holder = await (await browser.newContext()).newPage()
  const member = await (await browser.newContext()).newPage()

  async function signIn(page: Page, email: string) {
    const before = messages.length
    await page.goto(url)
    await page.getByLabel('Email address').fill(email)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.slice(before).find(message => /\b\d{8}\b/.test(message))).toBeTruthy()
    await page.getByLabel('Email code').fill(messages.slice(before).find(message => /\b\d{8}\b/.test(message))!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Sign out', exact: true, includeHidden: true })).toBeAttached()
  }

  try {
    const official = fixture.official(isolated.toString())
    await expect.poll(() => {
      expect(official.exitCode).toBeNull()
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }).toBe(true)
    await signIn(owner, `${schema}-owner@example.test`)
    await owner.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await owner.getByLabel('Installation claim code').inputValue()
    const installation = fixture.start('target/debug/leo', {
      DATA_DIR: join(fixture.root, 'data'),
      AGENT_HOME: join(fixture.root, 'home'),
      WORKSPACE_ROOTS: fixture.root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: code,
      LEO_INSTALLATION_NAME: 'Capacity installation',
    })
    await expect(async () => {
      expect(installation.exitCode).toBeNull()
      await owner.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(owner).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass()
    const installationUrl = owner.url()
    const session = await (await owner.request.get(`${url}/api/account/session`)).json()
    const installationId = session.installations[0].id
    for (const [page, suffix] of [[holder, 'holder'], [member, 'member']] as const) {
      const email = `${schema}-${suffix}@example.test`
      const invited = await owner.request.post(`${url}/api/installations/${installationId}/sharing/invitations`, {
        headers: { 'origin': url, 'x-csrf-token': session.csrf },
        data: { email },
      })
      expect(invited.status()).toBe(201)
      await signIn(page, email)
      await page.getByRole('button', { name: 'Accept invitation', exact: true }).click()
      await expect(page).toHaveURL(installationUrl)
    }

    // Capture the holder's actual authorized channel. No signaling, response or
    // budget is mocked; the incomplete declaration reaches the real decoder.
    await holder.addInitScript(() => {
      const create = RTCPeerConnection.prototype.createDataChannel
      RTCPeerConnection.prototype.createDataChannel = function (...args) {
        const channel = create.apply(this, args)
        Object.assign(window, { capacityChannel: channel })
        return channel
      }
    })
    await holder.reload()
    for (const page of [holder, member]) {
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 35000 })
    }

    await holder.evaluate(async () => {
      const channel = (window as typeof window & { capacityChannel: RTCDataChannel }).capacityChannel
      const fragment = (transfer: number, total: number, offset: number, payload: Uint8Array) => {
        const packet = new Uint8Array(13 + payload.length)
        const header = new DataView(packet.buffer)
        header.setUint8(0, 1)
        header.setUint32(1, transfer)
        header.setUint32(5, total)
        header.setUint32(9, offset)
        packet.set(payload, 13)
        channel.send(packet)
      }

      const total = Math.ceil(8_000_000 / 3) * 4 + 65536
      fragment(0xFFFF0001, total, 0, new TextEncoder().encode('{'))
      // The barrier proves the reservation has been accepted before the member
      // submits. Subsequent valid progress keeps it alive until page cleanup.
      await new Promise<void>((resolve, reject) => {
        const deadline = setTimeout(() => reject(new Error('Holder barrier timed out')), 2000)
        const receive = (event: MessageEvent) => {
          const packet = new Uint8Array(event.data)
          const header = new DataView(packet.buffer)
          if (header.getUint32(9) === 0 && header.getUint32(5) === packet.length - 13) {
            const frame = JSON.parse(new TextDecoder().decode(packet.subarray(13)))
            if (frame.id === 'capacity-barrier') {
              channel.removeEventListener('message', receive)
              clearTimeout(deadline)
              if (frame.status === 200)
                resolve()
              else
                reject(new Error('Holder barrier failed'))
            }
          }
        }

        channel.addEventListener('message', receive)
        const barrier = new TextEncoder().encode(JSON.stringify({
          type: 'request',
          id: 'capacity-barrier',
          account_id: '',
          role: 'member',
          method: 'GET',
          path: '/api/chats',
          headers: [],
          body: '',
        }))
        fragment(0xFFFF0002, barrier.length, 0, barrier)
      })
      let offset = 1
      setInterval(() => fragment(0xFFFF0001, total, offset++, new TextEncoder().encode(' ')), 1000)
    })

    await member.getByRole('link', { name: 'Missions', exact: true }).first().click()
    await member.getByRole('button', { name: 'New mission', exact: true }).click()
    await member.getByLabel('Mission name').fill('Large member mission')
    const prompt = 'x'.repeat(20000)
    await member.getByLabel('What should happen?').fill(prompt)
    await member.evaluate(() => {
      Object.assign(window, { capacityRoutes: [] })
      window.addEventListener('leo-transport-observation', (event) => {
        const observation = (event as CustomEvent).detail
        if (observation.method === 'POST' && observation.path === '/tasks')
          (window as typeof window & { capacityRoutes: string[] }).capacityRoutes.push(observation.route)
      })
    })
    await member.getByRole('button', { name: 'Create mission', exact: true }).click()
    await expect(member.getByRole('dialog')).toHaveCount(0)
    expect(await member.evaluate(() => (window as typeof window & { capacityRoutes: string[] }).capacityRoutes)).toEqual(['relay'])
    await expect(member.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct')
    const tasks = await (await member.request.get(`${url}/api/installations/${installationId}/api/tasks`)).json()
    const created = tasks.filter((task: { name: string }) => task.name === 'Large member mission')
    expect(created).toHaveLength(1)
    expect(created[0].prompt).toBe(prompt)
    await expect(holder.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct')
  }
  finally {
    await browser.close()
    await fixture.close()
    executeOfficialSql(database, `DROP SCHEMA ${schema} CASCADE`)
  }
})
