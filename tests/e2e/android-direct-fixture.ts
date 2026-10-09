import { chmod, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import process from 'node:process'
import { beaconRelayFixture } from './beacon-relay-fixture'

async function main() {
  // Device adapter for the existing real installation/control-plane fixture.
  // The output contains synthetic session credentials: never upload it as evidence.
  const output = process.argv[2]
  if (!output || !process.env.CAIRN_BEACON_TEST_DATABASE_URL)
    throw new Error('Provide a private output file and a disposable PostgreSQL database')

  process.env.CAIRN_BEACON_STUN_LISTEN ||= '0.0.0.0:3478'
  process.env.CAIRN_BEACON_STUN_URL ||= 'stun:10.0.2.2:3478'
  const network = process.env.CAIRN_NETWORK_ANDROID === 'true'
  const fixture = await beaconRelayFixture(4398, network ? '198.18.103.1' : '127.0.0.1')
  let cookie = ''
  let csrf = ''

  async function request(path: string, body?: unknown) {
    const response = await fetch(`${fixture.url}/api${path}`, {
      method: body === undefined ? 'GET' : 'POST',
      headers: {
        'origin': fixture.url,
        'cookie': cookie,
        'x-csrf-token': csrf,
        'content-type': 'application/json',
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    if (!response.ok)
      throw new Error(`Fixture ${path} returned ${response.status}`)
    const session = response.headers.get('set-cookie')?.match(/cairn_session=[^;]+/)
    if (session)
      cookie = session[0]
    return response.json()
  }

  async function until<T>(read: () => Promise<T>, ready: (value: T) => boolean): Promise<T> {
    const end = Date.now() + 30000
    while (Date.now() < end) {
      const value = await read()
      if (ready(value))
        return value
      await new Promise(resolve => setTimeout(resolve, 100))
    }

    throw new Error('Fixture readiness deadline exceeded')
  }

  try {
    const service = fixture.beacon()
    await until(() => fetch(`${fixture.url}/health`).then(response => response.ok).catch(() => false), Boolean)
    if (service.exitCode !== null)
      throw new Error('Beacon fixture exited')
    const challenge = await request('/account/email-code', { email: `android-direct-${Date.now()}@example.test` })
    await until(async () => fixture.messages.length, value => value > 0)
    const code = fixture.messages[0]!.match(/\b\d{8}\b/)![0]
    const session = await request('/account/verify', { challenge: challenge.challenge, code })
    csrf = session.csrf
    const claim = await request('/installations/claim-code', {})
    const installation = fixture.start('target/debug/cairn', {
      DATA_DIR: join(fixture.root, 'data'),
      AGENT_HOME: join(fixture.root, 'home'),
      WORKSPACE_ROOTS: fixture.root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      HOST: '127.0.0.1',
      PORT: '4399',
      CAIRN_BEACON_ORIGIN: fixture.url,
      CAIRN_INSTALLATION_CLAIM_CODE: claim.code,
      CAIRN_INSTALLATION_NAME: 'Android direct transport fixture',
      CAIRN_DIRECT_STUN_URLS: process.env.CAIRN_DIRECT_STUN_URLS || (network ? process.env.CAIRN_BEACON_STUN_URL : 'stun:127.0.0.1:3478'),
      // The emulator gateway forwards host UDP ports without changing their numbers.
      ...(network && !process.env.CAIRN_DIRECT_PUBLIC_IP
        ? {}
        : {
            CAIRN_DIRECT_PUBLIC_IP: process.env.CAIRN_DIRECT_PUBLIC_IP || '10.0.2.2',
          }),
    })
    const installations = await until(() => request('/installations'), values => values.some((value: { online: boolean }) => value.online))
    if (installation.exitCode !== null)
      throw new Error('Installation fixture exited')
    await writeFile(output, JSON.stringify({
      origin: fixture.url,
      installation: installations[0].id,
      cookie,
      csrf,
    }), { mode: 0o600 })
    await chmod(output, 0o600)
    process.stdout.write('Android real transport fixture ready (private configuration written)\n')
    await new Promise<void>((resolve) => {
      process.once('SIGTERM', resolve)
      process.once('SIGINT', resolve)
    })
  }
  finally {
    await fixture.close()
  }
}

main().catch((error: unknown) => {
  console.error(error)
  process.exitCode = 1
})
