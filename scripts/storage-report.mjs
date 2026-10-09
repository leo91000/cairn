// Run inside the runner: node --input-type=module < scripts/storage-report.mjs
// Uses the shipped Node runtime; reads metadata only, never credentials or chat content.
import { existsSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'
import process from 'node:process'
import { DatabaseSync } from 'node:sqlite'

const state = process.env.CAIRN_RUNNER_STATE || '/runner-state'
const data = process.env.DATA_DIR || '/data'
const db = new DatabaseSync(join(data, 'manager.db'), { readOnly: true })
const runs = new Map(db.prepare('SELECT id, status, data FROM runs').all().map(row => [row.id, { status: row.status, ...JSON.parse(row.data) }]))
const chats = new Set(db.prepare('SELECT json_extract(data,\'$.runId\') runId FROM records WHERE kind=\'chats\'').all().map(row => row.runId))
const allocated = path => statSync(path).blocks * 512
const disks = []
const legacy = { count: 0, allocatedBytes: 0, withoutChatBytes: 0 }

for (const id of readdirSync(join(state, 'disks'))) {
  const directory = join(state, 'disks', id)
  const raw = join(directory, 'data.ext4')
  if (existsSync(raw)) {
    const bytes = allocated(raw)
    legacy.count++
    legacy.allocatedBytes += bytes
    if (!chats.has(id))
      legacy.withoutChatBytes += bytes
  }

  const segmented = existsSync(join(directory, 'lazy', 'journal-v2.sqlite'))
  const journal = join(directory, 'lazy', segmented ? 'journal-v2.sqlite' : 'journal.sqlite')
  if (!existsSync(journal))
    continue
  const disk = new DatabaseSync(journal, { readOnly: true })
  try {
    disk.exec('BEGIN')
    const { generation, manifest } = disk.prepare('SELECT generation, manifest FROM state').get()
    const base = JSON.parse(manifest)
    // Segmented journal counters come from the controller's live accounting;
    // an offline inventory reports unknown rather than reading private payloads.
    const dirty = segmented ? { rows: null, bytes: runs.get(id)?.storage?.dirtyBytes ?? null } : disk.prepare('SELECT count(*) rows, coalesce(sum(end-start),0) bytes FROM writes').get()
    const { since } = segmented ? { since: runs.get(id)?.storage?.dirtySince ?? null } : disk.prepare('SELECT min(written_at) since FROM epochs').get()
    const free = disk.prepare('PRAGMA freelist_count').get().freelist_count * disk.prepare('PRAGMA page_size').get().page_size
    const autoVacuum = disk.prepare('PRAGMA auto_vacuum').get().auto_vacuum
    disk.exec('ROLLBACK')
    let journalBytes = 0
    let cacheBytes = 0
    for (const name of readdirSync(join(directory, 'lazy'))) {
      const path = join(directory, 'lazy', name)
      if (name === 'cache') {
        for (const block of readdirSync(path)) {
          // A clean cache block can be evicted during this read-only inventory.
          try {
            cacheBytes += allocated(join(path, block))
          }
          catch (error) {
            if (error.code !== 'ENOENT')
              throw error
          }
        }
      }
      else {
        try {
          journalBytes += allocated(path)
        }
        catch (error) {
          if (error.code !== 'ENOENT')
            throw error
        }
      }
    }

    const run = runs.get(id)
    disks.push({
      id,
      status: run?.status,
      logicalBytes: base.size,
      baseNonzeroBytes: base.blocks.filter(block => block.hash).reduce((sum, block) => sum + block.size, 0),
      journalBytes,
      cacheBytes,
      journalFreeBytes: free,
      autoVacuum,
      journalFormat: segmented ? 'segments-v2' : 'sqlite-blobs',
      generation,
      dirtyBytes: dirty.bytes,
      journalRows: dirty.rows,
      dirtyAgeSeconds: since === null ? null : Math.max(0, Math.round((Date.now() - since) / 1000)),
      waitingFor: run?.storage?.waitingFor ?? null,
      performance: run?.storage?.performance ?? null,
    })
  }
  finally {
    disk.close()
  }
}

db.close()
console.log(JSON.stringify({
  capturedAt: new Date().toISOString(),
  legacy,
  onDemand: {
    count: disks.length,
    allocatedBytes: disks.reduce((sum, disk) => sum + disk.journalBytes + disk.cacheBytes, 0),
    journalFreeBytes: disks.reduce((sum, disk) => sum + disk.journalFreeBytes, 0),
    logicalBytes: disks.reduce((sum, disk) => sum + disk.logicalBytes, 0),
  },
  disks,
}, null, 2))
