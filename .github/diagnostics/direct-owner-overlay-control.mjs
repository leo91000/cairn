import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, readlinkSync, lstatSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const product = '4969f8ed41956b569399e0aaac87384ca5da1d37'
const targets = ['tests/e2e/mcps.spec.ts', 'apps/web/tests/direct-transport.test.ts']
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
  for (const target of chosen)
    assert.equal(hash(originals[target]), manifest.files[target].originalSHA256, 'Immutable original hash mismatch')
  mkdirSync(evidence, { recursive: true })
  let knownBefore = true
  for (const target of chosen) {
    const file = join(root, target)
    const before = existsSync(file) ? readFileSync(file) : Buffer.alloc(0)
    const spec = manifest.files[target]
    const known = [spec.originalSHA256, spec.overlaySHA256].includes(hash(before))
    if (!known) {
      knownBefore = false
      writeFileSync(join(evidence, `unexpected-test-${targets.indexOf(target)}.private`), before)
    }
    writeFileSync(file, originals[target])
    assert.equal(hash(readFileSync(file)), spec.originalSHA256)
  }
  return { originalRestored: true, knownBefore, unitOnly, productQualified: false }
}
function pinnedOriginals(root) {
  return Object.fromEntries(targets.map(target => [target, git(root, 'show', `${product}:${target}`)]))
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
    writeFileSync(join(evidence, 'production-before.json'), JSON.stringify(trackedManifest(root)))
    writeFileSync(join(evidence, 'source-binding.json'), JSON.stringify({ product, tree: git(root, 'rev-parse', 'HEAD^{tree}').toString().trim(), observer: process.env.OBSERVER_SHA, diagnosticOnly: true, productQualified: false }))
    return
  }
  if (phase === 'apply') {
    checkRuntime(root, evidence)
    applyTestOverlays(root)
  }
  else if (phase === 'restore-unit' || phase === 'restore') {
    const receipt = restoreTestOverlays(root, evidence, pinnedOriginals(root), phase === 'restore-unit')
    writeFileSync(join(evidence, phase + '-receipt.json'), JSON.stringify(receipt))
    assert.equal(receipt.knownBefore, true, 'Unexpected test bytes preserved privately; restoration is non-green')
    checkRuntime(root, evidence)
    if (phase === 'restore')
      assert.equal(git(root, 'status', '--porcelain').toString().trim(), '')
  }
  else {
    checkRuntime(root, evidence)
    assert.equal(hash(readFileSync(join(root, targets[0]))), manifest.files[targets[0]].overlaySHA256)
    assert.equal(hash(readFileSync(join(root, targets[1]))), manifest.files[targets[1]].originalSHA256)
    assert.deepEqual(git(root, 'diff', '--name-only').toString().trim().split('\n'), [targets[0]])
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  assert(process.env.C40_EVIDENCE_DIR, 'Owned evidence directory required')
  runOverlayPhase(process.argv[2], process.cwd(), process.env.C40_EVIDENCE_DIR)
}
