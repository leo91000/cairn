import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

export async function awaitProjectImport({
  project,
  readOnly,
  inbox,
  report = message => process.stdout.write(`${message}\n`),
}) {
  const deadline = Date.now() + 30000
  while (!fs.existsSync(`${project}/sentinel`)) {
    assert.ok(Date.now() < deadline, 'lazy policy import timeout')
    await setTimeout(100)
  }

  assert.equal(fs.readFileSync(`${project}/sentinel`, 'utf8'), 'lazy')
  const write = () => fs.writeFileSync(`${project}/changed`, 'guest')
  if (readOnly)
    assert.throws(write, error => error.code === 'EROFS')
  else
    write()
  report('parallel.imported')
  // Publication makes files visible before sync and the host acknowledgement.
  // Keep the probe alive until its caller has received the import response.
  while (!fs.readFileSync(inbox, 'utf8').includes('"import-confirmed"')) {
    assert.ok(Date.now() < deadline, 'lazy policy acknowledgement timeout')
    await setTimeout(100)
  }

  report('parallel.done')
}

async function main() {
  const [workspace, project, sandbox] = process.argv.slice(2)
  const readOnly = sandbox === 'read-only'
  assert.equal(process.getuid(), 1000)
  assert.notEqual(spawnSync('sudo', ['-n', 'true']).status, 0)
  assert.equal(fs.readFileSync(`${workspace}/sentinel`, 'utf8'), sandbox)
  const write = () => fs.writeFileSync(`${workspace}/created`, 'guest only')
  if (readOnly)
    assert.throws(write, error => error.code === 'EROFS')
  else
    write()
  process.stdout.write('parallel.ready\n')
  await awaitProjectImport({ project, readOnly, inbox: '/run/leo-chat/messages.json' })
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
