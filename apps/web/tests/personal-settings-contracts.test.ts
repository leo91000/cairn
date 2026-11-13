import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { getInstallationOverview } from '../../../packages/contracts/generated/getInstallationOverview'
import { getInstallationSettings } from '../../../packages/contracts/generated/getInstallationSettings'
import overviewSchema from '../../../packages/contracts/generated/InstallationOverview.json'
import settingsSchema from '../../../packages/contracts/generated/InstallationSettings.json'
import { listAudit } from '../../../packages/contracts/generated/listAudit'
import auditSchema from '../../../packages/contracts/generated/ListAuditOutput.json'
import { markRead } from '../../../packages/contracts/generated/markRead'
import markSchema from '../../../packages/contracts/generated/MarkReadInput.json'
import { setOutcomeAside } from '../../../packages/contracts/generated/setOutcomeAside'
import asideSchema from '../../../packages/contracts/generated/SetOutcomeAsideInput.json'
import asideOutputSchema from '../../../packages/contracts/generated/SetOutcomeAsideOutput.json'
import corpus from '../../../tests/fixtures/personal-settings-contracts.json'

it('qualifies read marks, set-aside outcomes and redacted installation views', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const valid: [object, unknown[]][] = [
    [markSchema, corpus.marks],
    [asideSchema, corpus.asides],
    [asideOutputSchema, corpus.asideOutputs],
    [settingsSchema, corpus.settings],
    [overviewSchema, corpus.overviews],
    [auditSchema, corpus.audits],
  ]
  for (const [schema, values] of valid) {
    const validate = ajv.compile(schema)
    for (const value of values)
      expect(validate(value), JSON.stringify(validate.errors)).toBe(true)
  }

  const invalid: [object, unknown[]][] = [
    [markSchema, corpus.invalidMarks],
    [asideSchema, corpus.invalidAsides],
  ]
  for (const [schema, values] of invalid) {
    const validate = ajv.compile(schema)
    for (const value of values)
      expect(validate(value), JSON.stringify(value)).toBe(false)
  }
})

it('uses the generated personal and settings routes', async () => {
  const calls: unknown[] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return {} as T
  }

  await markRead(request, corpus.marks[0] as Parameters<typeof markRead>[1])
  await setOutcomeAside(request, corpus.asides[2] as Parameters<typeof setOutcomeAside>[1])
  await getInstallationSettings(request, {})
  await getInstallationOverview(request, {})
  await listAudit(request, {})
  expect(calls).toEqual([
    ['/reads', { method: 'POST', body: JSON.stringify(corpus.marks[0]) }],
    ['/dismissals', { method: 'POST', body: JSON.stringify(corpus.asides[2]) }],
    ['/settings', { method: 'GET' }],
    ['/overview', { method: 'GET' }],
    ['/audit', { method: 'GET' }],
  ])
})
