import assert from 'node:assert/strict'
import { createSocket } from 'node:dgram'
import { connect, createServer } from 'node:net'
import process from 'node:process'

const [mode, publicHost, privateHost] = process.argv.slice(2)
const response = 'network-ok'

function tcp(host, port) {
  return new Promise((resolve) => {
    const socket = connect({ host, port })
    let received = ''
    socket.setTimeout(3000)
    socket.on('data', data => received += data)
    socket.once('end', () => resolve(received === response))
    socket.once('error', () => resolve(false))
    socket.once('timeout', () => {
      socket.destroy()
      resolve(false)
    })
  })
}

function udp(host, port) {
  return new Promise((resolve) => {
    const socket = createSocket('udp4')
    const timer = setTimeout(finish, 3000, false)
    let finished = false

    function finish(result) {
      if (finished)
        return
      finished = true
      clearTimeout(timer)
      socket.close()
      resolve(result)
    }

    socket.once('error', () => finish(false))
    socket.once('message', data => finish(data.toString() === response))
    socket.send('probe', port, host)
  })
}

async function main() {
  if (mode === 'server') {
    for (const port of [22, 2222])
      createServer(socket => socket.end(response)).listen(port, '0.0.0.0')
    for (const port of [53, 2222]) {
      const socket = createSocket('udp4')
      socket.on('message', (_, peer) => socket.send(response, peer.port, peer.address))
      socket.bind(port, '0.0.0.0')
    }
  }
  else {
    assert.ok(['control', 'guest'].includes(mode))
    const control = mode === 'control'
    const checks = [
      [tcp, publicHost, 22, true],
      [tcp, publicHost, 2222, true],
      [tcp, privateHost, 22, control],
      [tcp, privateHost, 2222, control],
      [udp, publicHost, 53, true],
      [udp, publicHost, 2222, control],
      [udp, privateHost, 53, control],
    ]
    const results = await Promise.all(checks.map(([probe, host, port]) => probe(host, port)))
    checks.forEach(([probe, host, port, expected], index) => {
      assert.equal(results[index], expected, `${mode}: ${probe.name} ${host}:${port}`)
    })
    process.stdout.write(`network.${mode}.passed\n`)
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
