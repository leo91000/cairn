import {
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { setTimeout } from 'node:timers/promises'
import { expect, it } from 'vitest'
import { awaitProjectImport } from './fixtures/vm-policy.mjs'

it('keeps the guest alive after publication until the host confirms the import', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-policy-'))
  const project = path.join(root, 'project')
  const inbox = path.join(root, 'messages.json')
  await writeFile(inbox, '[]')
  let finished = false
  let observed
  const published = new Promise(resolve => observed = resolve)
  const operation = awaitProjectImport({
    project,
    readOnly: false,
    inbox,
    report: observed,
  })
    .then(() => finished = true)
  try {
    await mkdir(project)
    await writeFile(path.join(project, 'sentinel'), 'lazy')
    await published
    // Model the guest observing published files before the host receives the reply.
    await setTimeout(150)
    expect(finished).toBe(false)
    await writeFile(inbox, '[{"text":"import-confirmed"}]')
    await operation
    expect(finished).toBe(true)
  }
  finally {
    await writeFile(inbox, '[{"text":"import-confirmed"}]')
    await operation
    await rm(root, { recursive: true, force: true })
  }
})
