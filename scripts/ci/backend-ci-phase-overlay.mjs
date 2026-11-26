import { createHash } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const prelude = `import { writeFileSync as writeCIPhase } from 'node:fs'
function recordCIPhase(phase) {
  if (process.env.CAIRN_CI_PHASE_FILE)
    writeCIPhase(process.env.CAIRN_CI_PHASE_FILE, JSON.stringify({ phase }) + '\\n', { mode: 0o600 })
}
`
const anchors = [
  ['if (needsWorkerFixture) {\n', 'worker-fixture-build'],
  ['if (needsCodeWorker) {\n', 'code-worker-build'],
  ['const result = spawnSync(\'cargo\',', 'backend-tests'],
]

// Apply the same observer to an exact historical checkout without replacing
// its launcher or changing a Cargo argument, assertion or qualification input.
export function instrumentBackend(source) {
  if (source.includes('recordCIPhase'))
    throw new Error('Backend fixture is already observed')
  let observed = source
  const insertions = []
  for (const [anchor, phase] of anchors) {
    if (observed.split(anchor).length !== 2)
      throw new Error('Backend launcher shape changed; refusing diagnostic overlay')
    const before = anchor.startsWith('const result')
    const insertion = `${before ? '' : '  '}recordCIPhase('${phase}')\n`
    insertions.push([anchor, insertion, before])
    observed = observed.replace(anchor, before ? insertion + anchor : anchor + insertion)
  }

  for (const [anchor, insertion, before] of insertions)
    observed = observed.replace(before ? insertion + anchor : anchor + insertion, anchor)
  if (observed !== source)
    throw new Error('Backend diagnostic overlay was not insertion only')
  observed = source
  for (const [anchor, insertion, before] of insertions)
    observed = observed.replace(anchor, before ? insertion + anchor : anchor + insertion)
  return prelude + observed
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const file = process.argv[2]
  const source = readFileSync(file, 'utf8')
  const observed = instrumentBackend(source)
  writeFileSync(file, observed)
  const sha = value => createHash('sha256').update(value).digest('hex')
  console.log(JSON.stringify({
    observer: 'backend-phases',
    observationOnly: true,
    originalSha256: sha(source),
    observedSha256: sha(observed),
  }))
}
