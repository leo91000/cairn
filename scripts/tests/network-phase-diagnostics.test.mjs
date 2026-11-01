import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, it } from 'vitest'
import { networkPhaseDiagnostics } from '../network-phase-diagnostics.mjs'

it('keeps the last entered operation through lifecycle and cleanup events', () => {
  const directory = mkdtempSync(join(tmpdir(), 'cairn-network-phases-'))
  try {
    const output = join(directory, 'scenario.json')
    const phases = networkPhaseDiagnostics(output)
    phases.enter('browser-page')
    phases.event('page-closed')
    phases.cleanup('capture-transport')
    const report = JSON.parse(readFileSync(join(directory, 'scenario.phases.json'), 'utf8'))
    expect(report).toMatchObject({ testPhase: 'browser-page', cleanupPhase: 'capture-transport' })
    expect(report.entered).toMatchObject([{ phase: 'browser-page' }, { event: 'page-closed' }, { phase: 'capture-transport' }])
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

it('bounds retained phases and refuses arbitrary diagnostic content', () => {
  const directory = mkdtempSync(join(tmpdir(), 'cairn-network-phases-'))
  try {
    const output = join(directory, 'scenario')
    const phases = networkPhaseDiagnostics(output)
    for (let value = 0; value < 200; value++)
      phases.enter('rust-read')
    expect(() => phases.enter('private-cookie-and-payload')).toThrow('Unknown network qualification phase')
    expect(() => phases.cleanup('private-cookie-and-payload')).toThrow('Unknown network qualification phase')
    expect(() => phases.event('private-cookie-and-payload')).toThrow('Unknown network qualification lifecycle event')
    const encoded = readFileSync(`${output}.phases.json`, 'utf8')
    const report = JSON.parse(encoded)
    expect(report.entered).toHaveLength(128)
    expect(encoded).not.toContain('private')
    expect(report.entered.every(item => Number.isFinite(item.elapsedMs))).toBe(true)
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
})
