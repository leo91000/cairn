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
import snoozeSchema from '../../../packages/contracts/generated/SetConversationSnoozeInput.json'
import snoozeOutputSchema from '../../../packages/contracts/generated/SetConversationSnoozeOutput.json'
import { snoozeChat } from '../../../packages/contracts/generated/snoozeChat'
import corpus from '../../../tests/fixtures/personal-settings-contracts.json'

it('qualifies read marks, conversation snoozes and redacted installation views', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const valid: [object, unknown[]][] = [
    [markSchema, corpus.marks],
    [snoozeSchema, corpus.snoozes],
    [snoozeOutputSchema, corpus.snoozeOutputs],
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
    [snoozeSchema, corpus.invalidSnoozes],
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
  await snoozeChat(request, corpus.snoozes[1] as Parameters<typeof snoozeChat>[1])
  await getInstallationSettings(request, {})
  await getInstallationOverview(request, {})
  await listAudit(request, {})
  expect(calls).toEqual([
    ['/reads', { method: 'POST', body: JSON.stringify(corpus.marks[0]) }],
    ['/snoozes', { method: 'POST', body: JSON.stringify(corpus.snoozes[1]) }],
    ['/settings', { method: 'GET' }],
    ['/overview', { method: 'GET' }],
    ['/audit', { method: 'GET' }],
  ])
})
