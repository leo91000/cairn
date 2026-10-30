import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { checkStorage } from '../../../packages/contracts/generated/checkStorage'
import { getInstallationOverview } from '../../../packages/contracts/generated/getInstallationOverview'
import { getInstallationSettings } from '../../../packages/contracts/generated/getInstallationSettings'
import { getStorageSettings } from '../../../packages/contracts/generated/getStorageSettings'
import overviewSchema from '../../../packages/contracts/generated/InstallationOverview.json'
import settingsSchema from '../../../packages/contracts/generated/InstallationSettings.json'
import { listAudit } from '../../../packages/contracts/generated/listAudit'
import auditSchema from '../../../packages/contracts/generated/ListAuditOutput.json'
import { markRead } from '../../../packages/contracts/generated/markRead'
import markSchema from '../../../packages/contracts/generated/MarkReadInput.json'
import { saveStorageSettings } from '../../../packages/contracts/generated/saveStorageSettings'
import storageInputSchema from '../../../packages/contracts/generated/SaveStorageSettingsInput.json'
import { setOutcomeAside } from '../../../packages/contracts/generated/setOutcomeAside'
import asideSchema from '../../../packages/contracts/generated/SetOutcomeAsideInput.json'
import asideOutputSchema from '../../../packages/contracts/generated/SetOutcomeAsideOutput.json'
import storageSchema from '../../../packages/contracts/generated/StorageSettings.json'
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
    [storageSchema, corpus.storageViews],
    [storageInputSchema, corpus.storageInputs],
  ]
  for (const [schema, values] of valid) {
    const validate = ajv.compile(schema)
    for (const value of values)
      expect(validate(value), JSON.stringify(validate.errors)).toBe(true)
  }

  const invalid: [object, unknown[]][] = [
    [markSchema, corpus.invalidMarks],
    [asideSchema, corpus.invalidAsides],
    [storageInputSchema, corpus.invalidStorageInputs],
  ]
  for (const [schema, values] of invalid) {
    const validate = ajv.compile(schema)
    for (const value of values)
      expect(validate(value), JSON.stringify(value)).toBe(false)
  }

  // Storage views describe the configuration without any credential.
  expect(Object.keys(storageSchema.properties)).not.toContain('secretAccessKey')
  expect(Object.keys(storageSchema.properties)).not.toContain('accessKeyId')
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
  await getStorageSettings(request, {})
  await saveStorageSettings(request, corpus.storageInputs[1] as Parameters<typeof saveStorageSettings>[1])
  await checkStorage(request, {})
  expect(calls).toEqual([
    ['/reads', { method: 'POST', body: JSON.stringify(corpus.marks[0]) }],
    ['/dismissals', { method: 'POST', body: JSON.stringify(corpus.asides[2]) }],
    ['/settings', { method: 'GET' }],
    ['/overview', { method: 'GET' }],
    ['/audit', { method: 'GET' }],
    ['/settings/storage', { method: 'GET' }],
    ['/settings/storage', { method: 'PUT', body: JSON.stringify(corpus.storageInputs[1]) }],
    ['/settings/storage/check', { method: 'POST', body: '{}' }],
  ])
})
