import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { spawnSync } from 'node:child_process'
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { it, onTestFinished } from 'vitest'
import { parse } from 'yaml'
import { prepareBackendDependencies, verifyBackendDependencies } from '../ci-backend-dependencies.mjs'

function fixtureImports() {
  const sources = ['../../tests/fixtures/mcp-provider.ts', '../../tests/fixtures/mcp.mjs', '../../crates/installation/tests/mcps.rs']
  const imports = new Set(['tsx'])
  for (const file of sources) {
    const source = readFileSync(new URL(file, import.meta.url), 'utf8')
    for (const match of source.matchAll(/^import [^'";\n]*from ['"]([^'"]+)['"]/gm)) {
      if (!match[1].startsWith('node:') && !match[1].startsWith('.'))
        imports.add(match[1])
    }
  }

  return [...imports].sort()
}

const requiredImports = fixtureImports()
const packageName = specifier => specifier.startsWith('@') ? specifier.split('/').slice(0, 2).join('/') : specifier.split('/')[0]

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-dependencies-'))
  onTestFinished(() => rmSync(root, { recursive: true, force: true }))
  const observer = path.join(root, 'observer')
  const runnerTemp = path.join(root, 'runner')
  const candidate = path.join(runnerTemp, 'cairn-diagnostic-candidate')
  const modules = path.join(observer, 'node_modules')
  const store = path.join(modules, '.pnpm')
  for (const directory of [observer, candidate, store])
    mkdirSync(directory, { recursive: true })
  const names = [...new Set(requiredImports.map(packageName))]
  const meta = Object.fromEntries(['package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml']
    .map(file => [file, readFileSync(new URL(`../../${file}`, import.meta.url), 'utf8')]))
  const manifest = JSON.parse(meta['package.json'])
  const applicationLock = meta['pnpm-lock.yaml'].split(/^---[ \t]*\r?$/m).filter(part => part.trim()).at(-1).trim()
  const importer = parse(applicationLock).importers['.']
  for (const [name, value] of Object.entries(meta)) {
    writeFileSync(path.join(observer, name), value)
    writeFileSync(path.join(candidate, name), value)
  }

  writeFileSync(path.join(store, 'lock.yaml'), `${applicationLock}\n`)
  const installation = {
    packageManager: 'pnpm@12.3.4',
    nodeLinker: 'isolated',
    virtualStoreDir: '.pnpm',
    layoutVersion: 5,
    virtualStoreDirMaxLength: 120,
    included: { dependencies: true, devDependencies: true, optionalDependencies: true },
  }
  writeFileSync(path.join(modules, '.modules.yaml'), JSON.stringify(installation))
  const targets = {}
  for (const name of names) {
    const version = manifest.dependencies?.[name] ?? manifest.devDependencies[name]
    const locked = importer.dependencies?.[name] ?? importer.devDependencies[name]
    const identity = `${name}@${locked.version}`.replaceAll('/', '+').replaceAll(')(', '_').replaceAll('(', '_').replaceAll(')', '')
    const target = path.join(store, identity, 'node_modules', name)
    targets[name] = target
    mkdirSync(target, { recursive: true })
    const exports = name.endsWith('/server') ? { '.': './index.mjs', './stdio': './stdio.mjs' } : './index.mjs'
    writeFileSync(path.join(target, 'package.json'), JSON.stringify({
      name,
      version,
      type: 'module',
      exports,
    }))
    const code = name.endsWith('/server')
      ? `export class McpServer{constructor(info){this.info=info}registerTool(name,shape,call){this.tool=name;this.call=call}async connect(){console.log(JSON.stringify({name:this.info.name,tool:this.tool,out:await this.call({message:'probe'})}))}}`
      : name === 'zod' ? 'export const z={string:()=>null,object:value=>value}' : 'export const proof=true'
    writeFileSync(path.join(target, 'index.mjs'), code)
    writeFileSync(path.join(target, 'stdio.mjs'), 'export class StdioServerTransport{}')
    mkdirSync(path.dirname(path.join(modules, name)), { recursive: true })
    symlinkSync(target, path.join(modules, name), 'dir')
  }

  const observerSha = 'a'.repeat(40)
  const candidateSha = '3'.repeat(40)
  const source = {
    diagnosticOnly: true,
    qualificationClaim: false,
    observer: observerSha,
    revisions: { candidate: candidateSha },
    sources: { candidate },
  }
  const options = {
    observer,
    candidate,
    runnerTemp,
    observerSha,
    source,
    readHead: directory => directory === observer ? observerSha : candidateSha,
    readTracked: (directory, revision, file) => Buffer.from(meta[file]),
  }
  return {
    root,
    observer,
    candidate,
    modules,
    store,
    meta,
    installation,
    targets,
    options,
  }
}

function node(cwd, args) {
  return spawnSync(process.execPath, args, {
    cwd,
    encoding: 'utf8',
    env: { PATH: process.env.PATH, TEST_PREFIX: 'synthetic-' },
    timeout: 2000,
  })
}

it('goes red/green through the actual Node ESM resolver in the separate candidate without NODE_PATH', () => {
  const f = fixture()
  const args = ['--input-type=module', '-e', 'import \'tsx\'; import \'@modelcontextprotocol/client\';']
  const red = node(f.candidate, args)
  assert.equal(red.status, 1)
  assert.match(red.stderr, /ERR_MODULE_NOT_FOUND/)
  const proof = prepareBackendDependencies(f.options)
  assert.equal(node(f.candidate, args).status, 0)
  assert.deepEqual(verifyBackendDependencies(f.options, proof), proof)
})

it('covers every real provider/official-client/stdio import and records canonical ESM entrypoints', () => {
  const f = fixture()
  const args = ['--input-type=module', '-e', requiredImports.map(name => `import ${JSON.stringify(name)};`).join('\n')]
  assert.match(node(f.candidate, args).stderr, /ERR_MODULE_NOT_FOUND/)
  const proof = prepareBackendDependencies(f.options)
  assert.equal(node(f.candidate, args).status, 0)
  const recordedImports = Object.values(proof.packages).flatMap(entry => Object.keys(entry.entrypoints)).sort()
  assert.deepEqual(recordedImports, requiredImports)
  for (const [name, entry] of Object.entries(proof.packages)) {
    assert.equal(entry.packagePath, f.targets[name])
    for (const resolved of Object.values(entry.entrypoints)) {
      assert.ok(resolved.path.startsWith(`${entry.packagePath}/`))
      assert.match(resolved.sha256, /^[a-f0-9]{64}$/)
    }
  }

  assert.deepEqual(verifyBackendDependencies(f.options, proof), proof)
})

it.each(['@fastify/formbody', '@modelcontextprotocol/node', 'fastify'])('refuses missing real provider dependency at acquisition and startup:%s', (name) => {
  const f = fixture()
  const link = path.join(f.modules, name)
  unlinkSync(link)
  assert.throws(() => prepareBackendDependencies(f.options))
  symlinkSync(f.targets[name], link, 'dir')
  const proof = prepareBackendDependencies(f.options)
  const args = ['--input-type=module', '-e', `import ${JSON.stringify(name)}`]
  assert.equal(node(f.candidate, args).status, 0)
  unlinkSync(link)
  assert.match(node(f.candidate, args).stderr, /ERR_MODULE_NOT_FOUND/)
  assert.throws(() => verifyBackendDependencies(f.options, proof))
})

it.each(requiredImports)('refuses a missing required ESM export target at acquisition and startup:%s', (specifier) => {
  const f = fixture()
  const file = path.join(f.targets[packageName(specifier)], specifier.endsWith('/stdio') ? 'stdio.mjs' : 'index.mjs')
  const original = readFileSync(file)
  unlinkSync(file)
  assert.throws(() => prepareBackendDependencies(f.options))
  writeFileSync(file, original)
  const proof = prepareBackendDependencies(f.options)
  const args = ['--input-type=module', '-e', `import ${JSON.stringify(specifier)}`]
  assert.equal(node(f.candidate, args).status, 0)
  unlinkSync(file)
  assert.match(node(f.candidate, args).stderr, /ERR_MODULE_NOT_FOUND/)
  assert.throws(() => verifyBackendDependencies(f.options, proof))
})

it('refuses an unexported required stdio subpath even when its file exists', () => {
  const f = fixture()
  const file = path.join(f.targets['@modelcontextprotocol/server'], 'package.json')
  const manifest = JSON.parse(readFileSync(file))
  const original = readFileSync(file)
  delete manifest.exports['./stdio']
  const incomplete = JSON.stringify(manifest)
  writeFileSync(file, incomplete)
  assert.throws(() => prepareBackendDependencies(f.options))
  writeFileSync(file, original)
  const proof = prepareBackendDependencies(f.options)
  writeFileSync(file, incomplete)
  assert.throws(() => verifyBackendDependencies(f.options, proof))
})

it('rechecks Node resolution from the actual candidate fixture ancestry before admission', () => {
  const f = fixture()
  const proof = prepareBackendDependencies(f.options)
  const shadow = path.join(f.candidate, 'tests/fixtures/node_modules/tsx')
  mkdirSync(shadow, { recursive: true })
  copyFileSync(path.join(f.targets.tsx, 'package.json'), path.join(shadow, 'package.json'))
  writeFileSync(path.join(shadow, 'index.mjs'), 'export const shadow=true')
  assert.throws(() => verifyBackendDependencies(f.options, proof), /outside its acquired dependency context/)
})

it('refuses an inside-store retarget with identical manifest bytes before acquisition and after receipt', () => {
  const f = fixture()
  const target = f.targets.tsx
  const other = path.join(f.store, 'other-installation/node_modules/tsx')
  mkdirSync(other, { recursive: true })
  copyFileSync(path.join(target, 'package.json'), path.join(other, 'package.json'))
  writeFileSync(path.join(other, 'index.mjs'), 'console.log("switched-after-receipt")')
  const link = path.join(f.modules, 'tsx')
  unlinkSync(link)
  symlinkSync(other, link, 'dir')
  assert.throws(() => prepareBackendDependencies(f.options), /locked virtual-store package/)
  unlinkSync(link)
  symlinkSync(target, link, 'dir')
  const proof = prepareBackendDependencies(f.options)
  unlinkSync(link)
  symlinkSync(other, link, 'dir')
  const result = node(f.candidate, ['--input-type=module', '-e', 'import \'tsx\''])
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stdout, /switched-after-receipt/)
  assert.throws(() => verifyBackendDependencies(f.options, proof), /locked virtual-store package/)
})

it.each(['entrypoint-symlink', 'package-directory-symlink', 'changed-entrypoint', 'receipt-path'])('refuses changed required package bindings:%s', (scenario) => {
  const f = fixture()
  const proof = prepareBackendDependencies(f.options)
  const file = path.join(f.targets.tsx, 'index.mjs')
  if (scenario === 'entrypoint-symlink') {
    unlinkSync(file)
    symlinkSync(path.join(f.targets.zod, 'index.mjs'), file)
  }

  if (scenario === 'package-directory-symlink') {
    const replacement = path.join(f.store, 'replacement/node_modules/tsx')
    mkdirSync(replacement, { recursive: true })
    copyFileSync(path.join(f.targets.tsx, 'package.json'), path.join(replacement, 'package.json'))
    writeFileSync(path.join(replacement, 'index.mjs'), 'export const changed=true')
    rmSync(f.targets.tsx, { recursive: true })
    symlinkSync(replacement, f.targets.tsx, 'dir')
  }

  if (scenario === 'changed-entrypoint')
    writeFileSync(file, 'export const changed=true')
  if (scenario === 'receipt-path')
    proof.packages.tsx.packagePath = f.targets.zod
  assert.throws(() => verifyBackendDependencies(f.options, proof))
})

it.each(['specifier', 'version', 'peer-identity'])('uses the exact installed root importer rather than package manifest alone:%s', (scenario) => {
  const f = fixture()
  const before = f.meta['pnpm-lock.yaml']
  let after
  if (scenario === 'specifier')
    after = before.replace('specifier: 4.23.13', 'specifier: 4.23.12')
  if (scenario === 'version')
    after = before.replace('version: 4.23.13', 'version: 4.23.12')
  if (scenario === 'peer-identity')
    after = before.replace('version: 2.0.0(@modelcontextprotocol/server@2.0.0)(hono@4.13.7)', 'version: 2.0.0(@modelcontextprotocol/server@2.0.0)(hono@4.13.6)')
  assert.notEqual(after, before)
  f.meta['pnpm-lock.yaml'] = after
  for (const directory of [f.observer, f.candidate])
    writeFileSync(path.join(directory, 'pnpm-lock.yaml'), after)
  writeFileSync(path.join(f.store, 'lock.yaml'), after.split(/^---[ \t]*\r?$/m).filter(part => part.trim()).at(-1).trim())
  assert.throws(() => prepareBackendDependencies(f.options))
})

it('loads the unchanged real stdio fixture through the same verified candidate context', () => {
  const f = fixture()
  mkdirSync(path.join(f.candidate, 'tests/fixtures'), { recursive: true })
  const source = readFileSync(new URL('../../tests/fixtures/mcp.mjs', import.meta.url))
  writeFileSync(path.join(f.candidate, 'tests/fixtures/mcp.mjs'), source)
  assert.match(node(f.candidate, ['tests/fixtures/mcp.mjs']).stderr, /ERR_MODULE_NOT_FOUND/)
  prepareBackendDependencies(f.options)
  const result = node(f.candidate, ['tests/fixtures/mcp.mjs'])
  assert.equal(result.status, 0, result.stderr)
  assert.deepEqual(JSON.parse(result.stdout), { name: 'fixture-command', tool: 'fixture_echo', out: { content: [{ type: 'text', text: 'synthetic-probe' }] } })
  assert.deepEqual(readFileSync(path.join(f.candidate, 'tests/fixtures/mcp.mjs')), source)
})

it.each(['metadata', 'tracked', 'head', 'lock', 'version', 'incomplete', 'absent', 'root-symlink', 'package-escape', 'candidate-existing', 'candidate-link', 'root-path'])('refuses unsafe or unproven context:%s', (scenario) => {
  const f = fixture()
  if (scenario === 'metadata')
    writeFileSync(path.join(f.candidate, 'package.json'), '{}')
  if (scenario === 'tracked')
    f.options.readTracked = () => Buffer.from('different Git blob')
  if (scenario === 'head')
    f.options.readHead = () => 'f'.repeat(40)
  if (scenario === 'lock')
    writeFileSync(path.join(f.store, 'lock.yaml'), 'application: wrong')
  if (scenario === 'version')
    writeFileSync(path.join(f.targets.tsx, 'package.json'), JSON.stringify({ name: 'tsx', version: '2.0.0' }))
  if (scenario === 'incomplete')
    writeFileSync(path.join(f.modules, '.modules.yaml'), JSON.stringify({ ...f.installation, included: { dependencies: true, devDependencies: false } }))
  if (scenario === 'absent')
    rmSync(f.modules, { recursive: true })
  if (scenario === 'root-symlink') {
    rmSync(f.modules, { recursive: true })
    symlinkSync(f.root, f.modules, 'dir')
  }

  if (scenario === 'package-escape') {
    unlinkSync(path.join(f.modules, 'tsx'))
    symlinkSync(f.root, path.join(f.modules, 'tsx'), 'dir')
  }

  if (scenario === 'candidate-existing')
    mkdirSync(path.join(f.candidate, 'node_modules'))
  if (scenario === 'candidate-link')
    symlinkSync(f.modules, path.join(f.candidate, 'node_modules'), 'dir')
  if (scenario === 'root-path')
    f.options.candidate = path.join(f.root, 'wrong')
  assert.throws(() => prepareBackendDependencies(f.options))
})

it('revalidates the acquisition receipt and bridge instead of trusting setup step success', () => {
  const f = fixture()
  const proof = prepareBackendDependencies(f.options)
  assert.throws(() => verifyBackendDependencies(f.options, { ...proof, observer: 'f'.repeat(40) }))
  writeFileSync(path.join(f.modules, '.modules.yaml'), JSON.stringify({ ...f.installation, changedAfterAcquisition: true }))
  assert.throws(() => verifyBackendDependencies(f.options, proof))
})

it('keeps provisioning backend-only after setup and before the durable action/heavy command', () => {
  const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  const steps = workflow.jobs.backend.steps
  const setup = steps.findIndex(step => step.uses === './.github/actions/setup-pnpm')
  const provision = steps.findIndex(step => step.run === 'node scripts/ci-backend-dependencies.mjs')
  const action = steps.findIndex(step => step.uses === './.github/actions/backend-durable-evidence')
  assert.ok(setup < provision && provision < action)
  assert.equal(steps[provision].if, 'inputs.ci-diagnostic == \'backend\'')
  assert.ok(!workflow.jobs['native-runtime'].steps.some(step => step.run?.includes('ci-backend-dependencies')))
})
