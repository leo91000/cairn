import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, readlinkSync, lstatSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const product = '6edd796424f0a3054270c4a1a1fd2c60796e2e11'
export const historicalWorkflowReference = Object.freeze({
  commit: '7aac8c145f1e44d764c9f7150b01727fd0a1f19d',
  path: '.github/workflows/webkit-dependency-regression.yaml',
  gitBlob: 'b24561e7500adcea0e98926cfca3e210211b0518',
  sha256: '8df3a113886a9671b8dcf2c5c017f0a8903b73f590e1a8ffa5219ff0115953c4',
})
export function verifyHistoricalSourceAcquisition(workflow) {
  const fetch = 'git -c safe.directory="$GITHUB_WORKSPACE" fetch --depth=1 --no-tags origin "$OBSERVER_SHA" ' + historicalWorkflowReference.commit
  assert(workflow.split('\n').some(line => line.trim() === fetch), 'Historical source reference must be explicitly acquired before bind')
  const bind = workflow.indexOf('node test-results/direct-owner-receipt/direct-owner-overlay-control.mjs bind')
  assert(bind > workflow.indexOf(fetch), 'Historical source acquisition must precede binding')
}
export function historicalSourceWorkflow(root = process.cwd()) {
  const value = git(root, 'show', `${historicalWorkflowReference.commit}:${historicalWorkflowReference.path}`)
  assert.equal(hash(value), historicalWorkflowReference.sha256, 'Historical source bytes must match declared reference')
  assert.equal(git(root, 'rev-parse', `${historicalWorkflowReference.commit}:${historicalWorkflowReference.path}`).toString().trim(), historicalWorkflowReference.gitBlob)
  return value.toString()
}
const targets = ['tests/e2e/mcps.spec.ts', 'apps/web/tests/direct-transport.test.ts']
export const producerChainFiles = [
  'apps/web/beacon.html', 'apps/web/src/beacon.ts', 'apps/web/src/BeaconApp.vue',
  'apps/web/src/availability-read.ts', 'apps/web/src/document-lifetime.ts',
  'apps/web/src/api.ts', 'apps/web/src/installation-transport.ts',
  'apps/web/src/direct-channel.ts', 'apps/web/src/beacon-location.ts',
  'apps/web/vite.config.ts', 'tests/e2e/availability-probe.ts',
  'tests/e2e/mcps.spec.ts', 'tests/e2e/fixtures.ts',
  'tests/e2e/native-response-gate.ts', 'playwright.config.ts',
  'package.json', 'pnpm-lock.yaml',
]
export const observerChainFiles = [
  'archive-transparency.node.mjs', 'availability-reader-native.mjs',
  'skill-availability-probe.ts', 'b534-navigation-probe.ts',
  'direct-owner-probe.mjs', 'direct-owner-probe.d.mts',
  'direct-owner-probe.node.mjs', 'direct-owner-overlay-control.mjs',
  'closed-target-error.mjs', 'mcps.direct-owner.overlay.ts',
  'direct-transport.seam.overlay.ts', 'selected-workflow.yaml',
]
export const sourceGateCommands = [
  'node --test test-results/direct-owner-receipt/direct-owner-probe.node.mjs',
  'node test-results/direct-owner-receipt/archive-transparency.node.mjs --product-reference --source-trusted-limit',
]
export function selectedSourceGateScript(workflow) {
  const stepName = 'Light actual recorder projection overlay and cleanup checks (NOT browser)'
  const steps = workflow.split('      - name: ').filter(value => value.startsWith(stepName + '\n'))
  assert.equal(steps.length, 1, 'Exactly one selected source support gate is required')
  const lines = steps[0].split('\n')
  const start = lines.indexOf('        run: |')
  assert(start >= 0, 'Source support gate must declare the complete shell block')
  const commands = lines.slice(start + 1).filter(value => value.trim()).map((value) => {
    assert(value.startsWith('          '), 'Unexpected source gate nesting')
    return value.slice(10)
  })
  assert.deepEqual(commands, sourceGateCommands, 'Selected source gate must execute support and limit recognition explicitly')
  return commands.join('\n') + '\n'
}
const sourceDirectory = dirname(fileURLToPath(import.meta.url))
const hash = bytes => createHash('sha256').update(bytes).digest('hex')
const git = (root, ...args) => execFileSync('git', ['-c', `safe.directory=${root}`, ...args], { cwd: root })
const knownManifest = () => {
  const value = JSON.parse(readFileSync(join(sourceDirectory, 'overlay-manifest.json'), 'utf8'))
  assert.equal(value.product, product)
  assert.deepEqual(Object.keys(value.files).sort(), [...targets].sort())
  assert.equal(value.files[targets[0]].overlayFile, 'mcps.direct-owner.overlay.ts')
  assert.equal(value.files[targets[1]].overlayFile, 'direct-transport.seam.overlay.ts')
  return value
}
// Byte/source review precondition only: never a runtime producer certificate.
export function verifySourceTrustedChain(root, directory = sourceDirectory, manifest = knownManifest()) {
  const chain = manifest.sourceTrusted
  assert.equal(chain?.mode, 'PINNED_SOURCE_CONDITIONAL')
  assert.equal(chain.contractVersion, 'A81_SOURCE_TRUSTED_1')
  assert.equal(chain.rootDecisionSHA256, '8870ad8fd913883e366703bd3371304fdc11ed650e05f8dc6f61805f9a00d883')
  assert.equal(chain.universalRuntimeAdmission, false)
  assert.equal(chain.runtimeProducerReceipt, false)
  assert.equal(chain.runtimeProxyDetection, false)
  assert.equal(chain.installationMapping.origin, 'PINNED_WORKSPACE_FIXTURE_ID_SLUG')
  assert.equal(chain.installationMapping.fixture, 'tests/e2e/fixtures.ts')
  assert.deepEqual(chain.installationMapping.fields, ['workspace.installationId', 'workspace.installationSlug'])
  assert.equal(chain.productCommit, product)
  assert.equal(git(root, 'rev-parse', 'HEAD').toString().trim(), product)
  assert.equal(git(root, 'rev-parse', 'HEAD^{tree}').toString().trim(), chain.productTree)
  assert.deepEqual(Object.keys(chain.productFiles).sort(), [...producerChainFiles].sort())
  assert.deepEqual(Object.keys(chain.observerFiles).sort(), [...observerChainFiles].sort())
  assert.deepEqual(chain.selectedSourceGateCommands, sourceGateCommands)
  const workflow = readFileSync(join(directory, 'selected-workflow.yaml'), 'utf8')
  selectedSourceGateScript(workflow)
  verifyHistoricalSourceAcquisition(workflow)
  assert.deepEqual(chain.historicalWorkflowReference, historicalWorkflowReference)
  historicalSourceWorkflow(root)
  for (const file of producerChainFiles) {
    const spec = chain.productFiles[file]
    const original = git(root, 'show', `${product}:${file}`)
    assert.equal(hash(original), spec.sha256, 'Pinned producer-chain source hash mismatch')
    assert.equal(git(root, 'rev-parse', `${product}:${file}`).toString().trim(), spec.gitBlob)
    const actual = hash(readFileSync(join(root, file)))
    const overlay = manifest.files[file]?.overlaySHA256
    assert(actual === spec.sha256 || (overlay !== undefined && actual === overlay), 'Producer-chain bytes changed outside the exact known overlay')
  }
  for (const file of observerChainFiles) {
    const spec = chain.observerFiles[file]
    const bytes = readFileSync(join(directory, file))
    assert.equal(hash(bytes), spec.sha256, 'Pinned observer-chain source hash mismatch')
    assert.equal(execFileSync('git', ['-c', `safe.directory=${root}`, 'hash-object', '--stdin'], { input: bytes, cwd: root }).toString().trim(), spec.gitBlob)
  }
  return {
    mode: 'PINNED_SOURCE_CONDITIONAL', sourceChainBytesMatched: true,
    product, productTree: chain.productTree,
    producerFileCount: producerChainFiles.length, observerFileCount: observerChainFiles.length,
    runtimeProducerReceipt: false, universalRuntimeAdmission: false,
    runtimeProxyDetection: false, nativeQualified: false,
  }
}
export function validatedPlan(root) {
  const manifest = knownManifest()
  return targets.map((target) => {
    const spec = manifest.files[target]
    const original = readFileSync(join(root, target))
    const overlay = readFileSync(join(sourceDirectory, spec.overlayFile))
    assert.equal(hash(original), spec.originalSHA256, 'The exact pinned test must precede this overlay')
    assert.equal(hash(overlay), spec.overlaySHA256, 'Versioned overlay hash mismatch')
    return { target, overlay }
  })
}
export function applyTestOverlays(root) {
  const plan = validatedPlan(root) // Validate BOTH before any mutation.
  for (const { target, overlay } of plan)
    writeFileSync(join(root, target), overlay)
  return { diagnosticOnly: true, productQualified: false, changedTests: targets.length }
}
export function restoreTestOverlays(root, evidence, originals, unitOnly = false) {
  const manifest = knownManifest()
  const chosen = unitOnly ? [targets[1]] : targets
  let knownBefore = true
  const failures = []
  for (const target of chosen) {
    try {
      const spec = manifest.files[target]
      const original = typeof originals === 'function' ? originals(target) : originals[target]
      assert.equal(hash(original), spec.originalSHA256, 'Immutable original hash mismatch')

      const file = join(root, target)
      const before = existsSync(file) ? readFileSync(file) : Buffer.alloc(0)
      const known = [spec.originalSHA256, spec.overlaySHA256].includes(hash(before))
      if (!known) {
        knownBefore = false
        mkdirSync(evidence, { recursive: true })
        writeFileSync(join(evidence, `unexpected-test-${targets.indexOf(target)}.private`), before)
      }
      writeFileSync(file, original)
      assert.equal(hash(readFileSync(file)), spec.originalSHA256)
    }
    catch (error) {
      failures.push(error)
    }
  }
  if (failures.length === 1)
    throw failures[0]
  if (failures.length > 1)
    throw new AggregateError(failures, 'Independent test restoration failures', { cause: failures[0] })
  return { originalRestored: true, knownBefore, unitOnly, productQualified: false }
}
function pinnedOriginals(root) {
  return target => git(root, 'show', `${product}:${target}`)
}
function trackedManifest(root) {
  const files = git(root, 'ls-files', '-z').toString().split('\0').filter(value => value && !targets.includes(value))
  return Object.fromEntries(files.map((file) => {
    const name = join(root, file)
    return [file, hash(lstatSync(name).isSymbolicLink() ? readlinkSync(name) : readFileSync(name))]
  }))
}
function checkRuntime(root, evidence) {
  const before = JSON.parse(readFileSync(join(evidence, 'production-before.json'), 'utf8'))
  assert(Object.keys(before).length > 0)
  assert.deepEqual(trackedManifest(root), before, 'A product byte outside the two test overlays changed')
  assert.equal(git(root, 'rev-parse', 'HEAD').toString().trim(), product)
}
export function runOverlayPhase(phase, root, evidence) {
  assert(['bind', 'apply', 'restore-unit', 'check', 'restore'].includes(phase))
  mkdirSync(evidence, { recursive: true })
  const manifest = knownManifest()
  if (phase === 'bind') {
    assert.equal(git(root, 'rev-parse', 'HEAD').toString().trim(), product)
    assert.equal(git(root, 'status', '--porcelain').toString().trim(), '')
    validatedPlan(root)
    const chainReceipt = verifySourceTrustedChain(root)
    writeFileSync(join(evidence, 'source-trusted-chain.json'), JSON.stringify(chainReceipt))
    writeFileSync(join(evidence, 'production-before.json'), JSON.stringify(trackedManifest(root)))
    writeFileSync(join(evidence, 'source-binding.json'), JSON.stringify({ product, tree: git(root, 'rev-parse', 'HEAD^{tree}').toString().trim(), observer: process.env.OBSERVER_SHA, ownershipEvidenceScope: 'PINNED_SOURCE_CONDITIONAL', universalRuntimeAdmission: false, runtimeProducerReceipt: false, diagnosticOnly: true, productQualified: false }))
    return
  }
  if (phase === 'apply') {
    checkRuntime(root, evidence)
    verifySourceTrustedChain(root)
    applyTestOverlays(root)
  }
  else if (phase === 'restore-unit' || phase === 'restore') {
    const receipt = restoreTestOverlays(root, evidence, pinnedOriginals(root), phase === 'restore-unit')
    writeFileSync(join(evidence, phase + '-receipt.json'), JSON.stringify(receipt))
    assert.equal(receipt.knownBefore, true, 'Unexpected test bytes preserved privately; restoration is non-green')
    checkRuntime(root, evidence)
    verifySourceTrustedChain(root)
    if (phase === 'restore')
      assert.equal(git(root, 'status', '--porcelain').toString().trim(), '')
  }
  else {
    checkRuntime(root, evidence)
    verifySourceTrustedChain(root)
    assert.equal(hash(readFileSync(join(root, targets[0]))), manifest.files[targets[0]].overlaySHA256)
    assert.equal(hash(readFileSync(join(root, targets[1]))), manifest.files[targets[1]].originalSHA256)
    assert.deepEqual(git(root, 'diff', '--name-only').toString().trim().split('\n'), [targets[0]])
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  assert(process.env.C40_EVIDENCE_DIR, 'Owned evidence directory required')
  runOverlayPhase(process.argv[2], process.cwd(), process.env.C40_EVIDENCE_DIR)
}
