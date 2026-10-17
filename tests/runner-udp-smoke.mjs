import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'

// Run through the shipped controller, tap, conntrack, NAT and Firecracker guest.
export async function udpSmoke({
  root,
  docker,
  name,
  networkServer,
  publicPeer,
  api,
  until,
  storage,
}) {
  const id = randomUUID()
  const runId = randomUUID()
  const workspace = `/data/runs/${runId}/workspace`
  const source = path.join(root, 'data/runs', runId, 'workspace')
  await mkdir(source, { recursive: true })
  await writeFile(path.join(source, 'udp-policy.py'), await readFile(new URL('./fixtures/vm-udp-policy.py', import.meta.url)))
  const plan = {
    id,
    runId,
    expires: Date.now() + 180000,
    sandbox: 'yolo',
    cwd: workspace,
    command: ['sudo', 'python3', '-u', `${workspace}/udp-policy.py`, publicPeer],
    resources: { cpu: 1, memoryMiB: 1024, diskMiB: 1024 },
    storage,
    imports: [{ source: workspace, target: workspace }],
  }
  await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
  // Explicitly test the permissive host policy: an inbound DROP must still win.
  const policy = docker('exec', name, 'iptables', '-S', 'FORWARD').split('\n')[0].split(' ').at(-1)
  docker('exec', name, 'iptables', '-P', 'FORWARD', 'ACCEPT')
  let started = false
  let evidence

  async function output() {
    const logs = await readFile(path.join(root, 'state', `${id}.log`), 'utf8').catch(() => '')
    const text = logs.split('\n').filter(Boolean).map(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString()).join('')
    const exit = await readFile(path.join(root, 'state', `${id}.exit`), 'utf8').catch(() => null)
    assert.equal(exit, null, `UDP guest exited: ${text}`)
    return text
  }

  try {
    await api(`/runs/${id}`, 'POST')
    started = true
    const ready = await until(async () => (await output()).match(/udp.inbound.ready (10\.\d+\.\d+\.\d+)/))
    const guest = ready[1]
    const tap = docker('exec', name, 'ip', '-j', 'route', 'get', guest)
    const device = JSON.parse(tap)[0].dev
    assert.match(device, /^cairn\d+$/)
    const chain = device.toUpperCase()
    docker('exec', networkServer, 'ip', 'route', 'replace', `${guest}/32`, 'via', '93.184.216.2')
    docker('exec', networkServer, '/usr/local/bin/node', '-e', `
      const socket = require('node:dgram').createSocket('udp4');
      socket.send('unsolicited', 42000, ${JSON.stringify(guest)}, error => {
        if (error) throw error;
        socket.close();
      });
    `)
    // Check counters even when an outer network blocks the Internet STUN probe.
    await until(async () => (await output()).includes('udp.refill.passed'))
    const rules = docker('exec', name, 'iptables-save', '-c')
    const rows = rules.split('\n').filter(row => row.includes(`-A ${chain} `))
    const packets = row => Number(row.match(/^\[(\d+):/)[1])
    const count = fragment => packets(rows.find(row => row.includes(fragment)))
    assert.ok(count('! -s ') >= 1, 'spoofed source is dropped')
    const destinations = rows.filter(row => row.includes(' -d '))
    assert.equal(destinations.length, 14)
    // Linux may reject loopback/multicast as unroutable before FORWARD.
    // Assert the unicast fences that reach the forwarding hook.
    for (const range of ['10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '198.18.0.0/15', '203.0.113.0/24'])
      assert.ok(count(` -d ${range} `) >= 1, `reserved destination was dropped: ${range}`)
    const input = rules.split('\n').find(row => row.includes(`-A INPUT -i ${device} -j DROP`))
    assert.ok(packets(input) >= 2, 'gateway and public controller address are denied')
    const inbound = rules.split('\n').find(row => row.includes(`-A FORWARD -o ${device} -j DROP`))
    assert.ok(packets(inbound) >= 1, 'unsolicited datagram reached the inbound drop rule')
    const accepted = count(' --hashlimit-upto ')
    const dropped = count(' -p udp -j DROP')
    assert.match(rows.at(-1), / -j DROP$/, 'other protocols retain the default deny')
    const burstSeconds = Number((await output()).match(/udp.burst.sent ([\d.]+)/)[1])
    assert.ok(burstSeconds < 2, `burst must exceed the configured rate: ${burstSeconds}s`)
    const acceptanceCeiling = 110 + Math.ceil(50 * burstSeconds)
    assert.ok(accepted >= 100 && accepted <= acceptanceCeiling, `bounded new UDP acceptance: ${accepted} > ${acceptanceCeiling}`)
    assert.ok(dropped >= 500, `excess UDP flows dropped: ${dropped}`)
    evidence = {
      kind: 'kvm-public-udp-policy',
      guest,
      acceptedNew: accepted,
      droppedExcess: dropped,
      rules,
      output: await output(),
    }
    process.stdout.write(`${JSON.stringify(evidence)}\n`)
    await until(async () => (await output()).includes('udp.policy.passed'))
    evidence.output = await output()
    process.stdout.write(`${JSON.stringify(evidence)}\n`)
  }
  finally {
    if (started) {
      await api(`/runs/${id}`, 'DELETE')
      await until(async () => (await (await api('/health')).json()).activeRuns === 0)
    }

    docker('exec', name, 'iptables', '-P', 'FORWARD', policy)
  }
}
