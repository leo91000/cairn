// Identified retirement after a real guest ran, including controller restart.
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { attemptOutput } from './runner-probe.mjs'

export async function retirementSmoke({
  root,
  docker,
  name,
  api,
  until,
  reconnect,
  attempt,
}) {
  const plans = path.join(root, 'data/runner-plans')
  const old = JSON.parse(await readFile(path.join(plans, `${attempt}.json`), 'utf8'))
  const run = old.runId
  const retirementId = randomUUID()
  const endpoint = `/disks/${run}/delete`
  const log = `/runner-state/${attempt}.log`
  const owner = `/runner-state/${attempt}.run`
  const receipt = `/runner-state/disk-retirements/${run}/${retirementId}`
  const node = code => docker('exec', name, '/usr/local/bin/node', '-e', code)
  const exists = file => node(`console.log(require('node:fs').existsSync(${JSON.stringify(file)}))`) === 'true'
  const retire = () => api(endpoint, 'POST', { retirementId, attempts: [attempt] })
  const restart = async () => {
    docker('restart', name)
    docker('exec', '-d', name, '/usr/local/bin/node', '/data/storage-fixture/server.mjs')
    await reconnect()
  }

  assert.ok(exists(log), 'the stopped guest must have real output to erase')
  assert.equal(node(`console.log(require('node:fs').readFileSync(${JSON.stringify(owner)},'utf8'))`), run)
  node(`const fs=require('node:fs');fs.renameSync(${JSON.stringify(log)},${JSON.stringify(`${log}.saved`)});fs.mkdirSync(${JSON.stringify(log)});`)
  // Emulate the previous controller's failed deletion: it removed its mappings
  // before failing on the final private log. The manager still knows the attempt.
  node(`const fs=require('node:fs');const prefix='/runner-state/${attempt}';
    if(fs.existsSync(prefix+'.vm.json')){const vm=JSON.parse(fs.readFileSync(prefix+'.vm.json','utf8')).vmId;
      for(const suffix of ['boot.log','vmm.log'])fs.rmSync('/runner-state/'+vm+'.'+suffix,{force:true});}
    fs.rmSync(prefix+'.run',{force:true});fs.rmSync(prefix+'.vm.json',{force:true});
    fs.rmSync('/data/runner-plans/${attempt}.json',{force:true});`)
  await api(endpoint, 'POST', { retirementId, attempts: [attempt] }, 500)
  assert.equal(exists(receipt), false, 'failed cleanup cannot acknowledge retirement')
  assert.ok(exists(owner), 'failed log erasure must retain its run-to-attempt identity')
  // Ordinary manager worker release can erase this shared plan independently.
  await rm(path.join(plans, `${attempt}.json`), { force: true })
  node(`const fs=require('node:fs');fs.rmdirSync(${JSON.stringify(log)});fs.renameSync(${JSON.stringify(`${log}.saved`)},${JSON.stringify(log)});`)
  await restart()
  const retired = await (await retire()).json()
  assert.equal(retired.deleted, true)
  assert.equal(retired.retirementId, retirementId)
  assert.ok(exists(receipt))
  assert.equal(exists(log), false)
  assert.equal(exists(owner), false)
  assert.equal(exists(`/data/runner-plans/${attempt}.json`), false)
  // The next incarnation must be safe even when the receipt is reopened.
  await restart()

  const fresh = randomUUID()
  const canary = `${old.cwd}/new-environment`
  const plan = {
    ...old,
    id: fresh,
    expires: Date.now() + 180000,
    command: ['/usr/local/bin/node', '-e', `
      const fs=require('node:fs'),assert=require('node:assert/strict');
      assert.equal(fs.existsSync(${JSON.stringify(`${old.cwd}/old-environment`)}),false,'old working files were discarded');
      const file=fs.openSync(${JSON.stringify(canary)},'w');
      fs.writeFileSync(file,'new incarnation');fs.fsyncSync(file);fs.closeSync(file);
      const directory=fs.openSync(${JSON.stringify(old.cwd)},'r');fs.fsyncSync(directory);fs.closeSync(directory);
      console.log('retirement.new.ready');
      setInterval(()=>console.log('retirement.new.tick '+fs.readFileSync(${JSON.stringify(canary)},'utf8')),200);
    `],
  }
  await writeFile(path.join(plans, `${fresh}.json`), JSON.stringify(plan))
  const output = () => attemptOutput(JSON.parse(node(`
    const fs=require('node:fs');
    console.log(JSON.stringify(fs.readFileSync('/runner-state/${fresh}.log','utf8').trim().split('\\n').filter(Boolean).map(line=>JSON.parse(line))));
  `)))
  try {
    await api(`/runs/${fresh}`, 'POST')
    await until(() => {
      try {
        return output().includes('retirement.new.ready')
      }
      catch { return false }
    })
    const before = output().split('retirement.new.tick').length
    const repeated = await (await retire()).json()
    assert.equal(repeated.retirementId, retirementId)
    assert.equal(repeated.deleted, true)
    await until(() => output().split('retirement.new.tick').length > before)
    assert.ok(output().includes('retirement.new.tick new incarnation'))
    assert.equal((await (await api('/health')).json()).activeRuns, 1, 'an old receipt must not evict the new VM')
  }
  finally {
    await api(`/runs/${fresh}`, 'DELETE')
  }

  await api(`/runs/${fresh}/wait`, 'POST')
  const inactive = await (await retire()).json()
  assert.equal(inactive.retirementId, retirementId)
  assert.ok(exists(`/runner-state/${fresh}.log`), 'the completed replacement keeps its output')
  assert.ok(exists(`/runner-state/${fresh}.run`), 'the completed replacement keeps its disk ownership')
  const reopened = randomUUID()
  await writeFile(path.join(plans, `${reopened}.json`), JSON.stringify({
    ...plan,
    id: reopened,
    expires: Date.now() + 180000,
    command: ['/usr/local/bin/node', '-e', `
      const fs=require('node:fs'),assert=require('node:assert/strict');
      assert.equal(fs.readFileSync(${JSON.stringify(canary)},'utf8'),'new incarnation');
      console.log('retirement.inactive.preserved');
    `],
  }))
  await api(`/runs/${reopened}`, 'POST')
  assert.equal((await (await api(`/runs/${reopened}/wait`, 'POST')).json()).StatusCode, 0, 'an old receipt preserves the completed replacement disk')
  await api(`/runs/${reopened}`, 'DELETE')
  await api(endpoint, 'POST', { retirementId: randomUUID() })
  process.stdout.write(`${JSON.stringify({
    mode: 'identified-vm-retirement-retry-restart',
    run,
    attempt,
    fresh,
    status: 'passed',
  })}\n`)
}
